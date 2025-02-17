//! Proxy login and base app used for debugging exchanged messages.

use std::net::{SocketAddr, SocketAddrV4};
use std::{fmt, fs, io, thread};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::path::PathBuf;
use std::io::Write;
use std::fs::File;

use tracing::{error, info, instrument, warn};

use flate2::read::ZlibDecoder;
use blowfish::Blowfish;
use rsa::{RsaPrivateKey, RsaPublicKey};

use wgtk::net::element::{DebugElementUndefined, DebugElementVariable16, SimpleElement};
use wgtk::net::bundle::{Bundle, NextElementReader, ElementReader};

use wgtk::net::app::{login, base, client, proxy};
use wgtk::net::app::common::entity::Entity;
use wgtk::net::app::proxy::PacketDirection;

use wgtk::util::io::serde_pickle_de_options;

use crate::CliResult;
use super::gen;


pub fn run(
    login_app_addr: SocketAddrV4,
    real_login_app_addr: SocketAddrV4,
    base_app_addr: SocketAddrV4,
    encryption_key: Option<Arc<RsaPrivateKey>>,
    real_encryption_key: Option<Arc<RsaPublicKey>>,
) -> CliResult<()> {

    let mut login_app = login::proxy::App::new(login_app_addr.into(), real_login_app_addr.into(), real_encryption_key)
        .map_err(|e| format!("Failed to bind login app: {e}"))?;
    
    if let Some(encryption_key) = encryption_key {
        login_app.set_encryption(encryption_key);
    }

    login_app.set_forced_base_app_addr(base_app_addr);

    let base_app = proxy::App::new(base_app_addr.into())
        .map_err(|e| format!("Failed to bind base app: {e}"))?;

    let dump_dir = PathBuf::from("proxy-dump");
    let _ = fs::remove_dir_all(&dump_dir);
    fs::create_dir_all(&dump_dir).map_err(|e| format!("Failed to create proxy dump directory: {e}"))?;

    let shared = Arc::new(Shared {
        dump_dir,
        pending_clients: Mutex::new(HashMap::new()),
    });

    let login_thread = LoginThread {
        app: login_app,
        shared: Arc::clone(&shared),
    };

    let base_thread = BaseThread {
        app: base_app,
        shared,
        next_tick: None,
        entities: HashMap::new(),
        selected_entity_id: None,
        player_entity_id: None,
        partial_resources: HashMap::new(),
    };
    
    thread::scope(move |scope| {
        scope.spawn(move || login_thread.run());
        scope.spawn(move || base_thread.run());
    });

    Ok(())

}


#[derive(Debug)]
struct LoginThread {
    app: login::proxy::App,
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct BaseThread {
    app: proxy::App,
    shared: Arc<Shared>,
    next_tick: Option<u8>,
    entities: HashMap<u32, &'static EntityType>,
    selected_entity_id: Option<u32>,
    player_entity_id: Option<u32>,
    partial_resources: HashMap<u16, PartialResource>,
}

#[derive(Debug)]
struct Shared {
    dump_dir: PathBuf,
    pending_clients: Mutex<HashMap<SocketAddr, PendingClient>>,
}

#[derive(Debug)]
struct PendingClient {
    base_app_addr: SocketAddrV4,
    blowfish: Arc<Blowfish>,
}

/// Describe a partial resource being download, a header must have been sent.
#[derive(Debug)]
struct PartialResource {
    /// The byte description sent in the resource header.
    description: Vec<u8>,
    /// The next sequence number expected, any other sequence number abort the download
    /// with an error.
    sequence_num: u8,
    /// The full assembled data.
    data: Vec<u8>,
}

impl LoginThread {

    #[instrument(name = "login", skip_all)]
    fn run(mut self) {

        use login::proxy::Event;

        info!("Running on: {}", self.app.addr().unwrap());
        
        if self.app.has_encryption() {
            info!("Encryption enabled");
        }

        loop {
            match self.app.poll() {
                Event::IoError(error) => {
                    if let Some(addr) = error.addr {
                        warn!(%addr, "Error: {}", error.error);
                    } else {
                        warn!("Error: {}", error.error);
                    }
                }
                Event::Ping(ping) => {
                    info!(addr = %ping.addr, "Ping-Pong: {:?}", ping.latency);
                }
                Event::LoginSuccess(success) => {
                    info!(addr = %success.addr, "Login success");
                    self.shared.pending_clients.lock().unwrap().insert(success.addr, PendingClient { 
                        base_app_addr: success.real_base_app_addr,
                        blowfish: success.blowfish, 
                    });
                }
                Event::LoginError(error) => {
                    info!(addr = %error.addr, "Login error: {:?}", error.error);
                }
            }
        }

    }

}

impl BaseThread {

    #[instrument(name = "base", skip_all)]
    fn run(mut self) {

        use proxy::Event;

        info!("Running on: {}", self.app.addr().unwrap());

        loop {
            match self.app.poll() {
                Event::IoError(error) => {
                    if let Some(addr) = error.addr {
                        warn!(%addr, "Error: {}", error.error);
                    } else {
                        warn!("Error: {}", error.error);
                    }
                }
                Event::Rejection(rejection) => {
                    if let Some(pending_client) = self.shared.pending_clients.lock().unwrap().remove(&rejection.addr) {
                        
                        info!("Rejection of known peer: {} (to {})", rejection.addr, pending_client.base_app_addr);
                        
                        self.app.bind_peer(
                            rejection.addr, 
                            SocketAddr::V4(pending_client.base_app_addr), 
                            Some(pending_client.blowfish),
                            None).unwrap();

                    } else {
                        warn!("Rejection of unknown peer: {}", rejection.addr);
                    }
                }
                Event::Bundle(bundle) => {
                    
                    let res = match bundle.direction {
                        PacketDirection::Out => self.read_out_bundle(bundle.bundle, bundle.addr),
                        PacketDirection::In => self.read_in_bundle(bundle.bundle, bundle.addr),
                    };

                    if let Err(e) = res {
                        error!(addr = %bundle.addr, "Error while reading bundle: ({:?}) {e}", bundle.direction);
                    }

                }
                    
            }
        }

    }

    fn read_out_bundle(&mut self, bundle: Bundle, addr: SocketAddr) -> io::Result<()> {

        let mut reader = bundle.element_reader();
        while let Some(elt) = reader.next() {
            match elt {
                NextElementReader::Element(elt) => {
                    if !self.read_out_element(elt, addr)? {
                        break;
                    }
                }
                NextElementReader::Reply(reply) => {
                    let request_id = reply.request_id();
                    let _elt = reply.read_simple::<()>()?;
                    warn!(%addr, "-> Reply #{request_id}");
                    break;
                }
            }
        }

        Ok(())

    }

    fn read_out_element(&mut self, elt: ElementReader, addr: SocketAddr) -> io::Result<bool> {
        
        use base::element::*;

        match elt.id() {
            // LoginKey::ID => {}  // This should not be encrypted so we just ignore it!
            SessionKey::ID => {
                let elt = elt.read_simple::<SessionKey>()?;
                info!(%addr, "-> Session key: 0x{:08X}", elt.element.session_key);
            }
            EnableEntities::ID => {
                let _ee = elt.read_simple::<EnableEntities>()?;
                info!(%addr, "-> Enable entities");
            }
            DisconnectClient::ID => {
                let dc = elt.read_simple::<DisconnectClient>()?;
                info!(%addr, "-> Disconnect: 0x{:02X}", dc.element.reason);
            }
            id if id::BASE_ENTITY_METHOD.contains(id) => {

                // Account::doCmdInt3 (AccountCommands.CMD_SYNC_DATA), exposed id: 0x0E, message id: 0x95

                if let Some(entity_id) = self.player_entity_id {
                    // Unwrap because selected entity should exist!
                    let entity_type = *self.entities.get(&entity_id).unwrap();
                    return (entity_type.base_entity_method)(&mut *self, addr, entity_id, elt);
                }

                let elt = elt.read_simple::<DebugElementUndefined<0>>()?;
                warn!(%addr, "-> Base entity method (unknown selected entity): msg#{} {:?} (request: {:?})", id - id::BASE_ENTITY_METHOD.first, elt.element, elt.request_id);
                return Ok(false);

            }
            id => {
                let elt = elt.read_simple::<DebugElementUndefined<0>>()?;
                error!(%addr, "-> Element #{id} {:?} (request: {:?})", elt.element, elt.request_id);
                return Ok(false);
            }
        }

        Ok(true)

    }

    fn read_in_bundle(&mut self, bundle: Bundle, addr: SocketAddr) -> io::Result<()> {

        let mut reader = bundle.element_reader();
        while let Some(elt) = reader.next() {
            match elt {
                NextElementReader::Element(elt) => {
                    if !self.read_in_element(elt, addr)? {
                        break;
                    }
                }
                NextElementReader::Reply(reply) => {
                    let request_id = reply.request_id();
                    let _elt = reply.read_simple::<()>()?;
                    warn!(%addr, "<- Reply #{request_id}");
                    break;
                }
            }
        }

        Ok(())

    }

    fn read_in_element(&mut self, mut elt: ElementReader, addr: SocketAddr) -> io::Result<bool> {

        use client::element::*;

        match elt.id() {
            UpdateFrequencyNotification::ID => {
                let ufn = elt.read_simple::<UpdateFrequencyNotification>()?;
                info!(%addr, "<- Update frequency: {} Hz, game time: {}", ufn.element.frequency, ufn.element.game_time);
            }
            TickSync::ID => {
                let ts = elt.read_simple::<TickSync>()?;
                if let Some(next_tick) = self.next_tick {
                    if next_tick != ts.element.tick {
                        warn!(%addr, "<- Tick missed, expected {next_tick}, got {}", ts.element.tick);
                    }
                }
                self.next_tick = Some(ts.element.tick.wrapping_add(1));
            }
            ResetEntities::ID => {

                let re = elt.read_simple::<ResetEntities>()?;

                info!(%addr, "<- Reset entities, keep player on base: {}, entities: {}", 
                    re.element.keep_player_on_base, self.entities.len());

                // Don't delete player entity if requested...
                let mut player_entity = None;
                if re.element.keep_player_on_base {
                    if let Some(player_entity_id) = self.player_entity_id {
                        player_entity = Some(self.entities.remove_entry(&player_entity_id).unwrap());
                    }
                }
                
                self.entities.clear();
                self.player_entity_id = None;
                
                // Restore player entity!
                if let Some((player_entity_id, player_entity)) = player_entity {
                    self.entities.insert(player_entity_id, player_entity);
                    self.player_entity_id = Some(player_entity_id);
                }

            }
            LoggedOff::ID => {
                let lo = elt.read_simple::<LoggedOff>()?;
                info!(%addr, "<- Logged off: 0x{:02X}", lo.element.reason);
            }
            CreateBasePlayerHeader::ID => {

                let cbp = elt.read_simple_stable::<CreateBasePlayerHeader>()?;

                if let Some(entity_type) = cbp.element.entity_type_id.checked_sub(1).and_then(|i| ENTITY_TYPES.get(i as usize)) {
                    self.entities.insert(cbp.element.entity_id, entity_type);
                    self.player_entity_id = Some(cbp.element.entity_id);
                    return (entity_type.create_base_player)(&mut *self, addr, elt);
                }

                self.player_entity_id = None;
                // It's possible to skip it because its len is variable.
                let dbg = elt.read_simple::<DebugElementVariable16<0>>()?;
                warn!(%addr, "<- Create base player with invalid entity type id: 0x{:02X}, {:?}", 
                    cbp.element.entity_type_id, dbg.element);

            }
            CreateCellPlayer::ID => {
                let ccp = elt.read_simple::<CreateCellPlayer>()?;
                warn!(%addr, "<- Create cell player: {:?}", ccp.element);
            }
            SelectPlayerEntity::ID => {
                let _spe = elt.read_simple::<SelectPlayerEntity>()?;
                if let Some(player_entity_id) = self.player_entity_id {
                    info!(%addr, "<- Select player entity: {player_entity_id}");
                } else {
                    warn!(%addr, "<- Select player entity: no player entity")
                }
                self.selected_entity_id = self.player_entity_id;
            }
            ResourceHeader::ID => {

                let rh = elt.read_simple::<ResourceHeader>()?;
                info!(%addr, "<- Resource header: {}", rh.element.id);

                // Intentionally overwrite any previous downloading resource!
                self.partial_resources.insert(rh.element.id, PartialResource {
                    description: rh.element.description,
                    sequence_num: 0,
                    data: Vec::new(),
                });

            }
            ResourceFragment::ID => {

                let rf = elt.read_simple::<ResourceFragment>()?;
                let res_id = rf.element.id;

                let Some(partial_resource) = self.partial_resources.get_mut(&res_id) else {
                    warn!(%addr, "<- Resource fragment: {res_id}, len: {}, missing header", rf.element.data.len());
                    return Ok(true);
                };

                if rf.element.sequence_num != partial_resource.sequence_num {
                    // Just forgetting about the resource!
                    warn!(%addr, "<- Resource fragment: {res_id}, len: {}, invalid sequence number, expected {}, got {}", 
                    rf.element.data.len(), partial_resource.sequence_num, rf.element.sequence_num);
                    let _ = self.partial_resources.remove(&res_id);
                    return Ok(true);
                }

                partial_resource.sequence_num += 1;
                partial_resource.data.extend_from_slice(&rf.element.data);
                info!(%addr, "<- Resource fragment: {res_id}, len: {}, sequence number: {}", 
                    rf.element.data.len(), partial_resource.sequence_num);
                
                // Process the finished fragment!
                if rf.element.last {

                    let resource = self.partial_resources.remove(&rf.element.id).unwrap();
                    
                    // See: scripts/client/game.py#L223
                    let (total_len, crc32) = match serde_pickle::value_from_reader(&resource.description[..], serde_pickle_de_options()) {
                        Ok(serde_pickle::Value::Tuple(values)) if values.len() == 2 => {
                            if let &[serde_pickle::Value::I64(total_len), serde_pickle::Value::I64(crc32)] = &values[..] {
                                (total_len as u32, crc32 as u32)
                            } else {
                                warn!(%addr, "<- Invalid resource description: unexpected values: {values:?}");
                                return Ok(true);
                            }
                        }
                        Ok(v) => {
                            warn!(%addr, "<- Invalid resource description: python: {v}");
                            return Ok(true);
                        }
                        Err(e) => {
                            warn!(%addr, "<- Invalid resource description: {e}");
                            return Ok(true);
                        }
                    };

                    let actual_total_len = resource.data.len();
                    if actual_total_len != total_len as usize {
                        warn!(%addr, "<- Invalid resource length, expected: {total_len}, got: {actual_total_len}");
                        return Ok(true);
                    }

                    let actual_crc32 = crc32fast::hash(&resource.data);
                    if actual_crc32 != crc32 {
                        warn!(%addr, "<- Invalid resource crc32, expected: 0x{crc32:08X}, got: 0x{actual_crc32:08X}");
                        return Ok(true);
                    }

                    info!(%addr, "<- Resource completed: {res_id}, len: {actual_total_len}, crc32: 0x{crc32:08X}");

                    // TODO: The full data looks like to be a zlib-compressed pickle.
                    // TODO: onCmdResponse for requested SYNC use RES_SUCCESS=0, RES_STREAM=1, RES_CACHE=2 for result_id
                    //       When RES_STREAM is used, then a resource (header+fragment) is expected with the associated request_id.

                    match serde_pickle::value_from_reader(ZlibDecoder::new(&resource.data[..]), serde_pickle_de_options()) {
                        Ok(val) => {
                            
                            let dump_file = self.shared.dump_dir.join(format!("res_{crc32:08x}.txt"));
                            info!(%addr, "<- Saving resource to: {}", dump_file.display());

                            let mut dump_writer = File::create(dump_file).unwrap();
                            write!(dump_writer, "{val}").unwrap();

                        }
                        Err(e) => {

                            warn!(%addr, "<- Resource: python error: {e}");

                            // FIXME: It appears that the current serde-pickle impl doesn't
                            // support recursive structures, however the structure that is 
                            // initially requested with 'CMD_SYNC_DATA' contains some.
                            // FIXME: The resource that is received by the from the chat
                            // command contains a "deque" object, which cannot be parsed
                            // so we get a "unresolved global reference" error.

                            let raw_file = self.shared.dump_dir.join(format!("res_{crc32:08x}.raw"));
                            info!(%addr, "<- Saving resource to: {}", raw_file.display());

                            let mut raw_writer = File::create(raw_file).unwrap();
                            std::io::copy(&mut ZlibDecoder::new(&resource.data[..]), &mut raw_writer).unwrap();

                        }
                    }

                }

            }
            id if id::ENTITY_METHOD.contains(id) => {

                // Account::msg#37 = onClanInfoReceived
                // Account::msg#39 = showGUI

                if let Some(entity_id) = self.selected_entity_id {
                    // Unwrap because selected entity should exist!
                    let entity_type = *self.entities.get(&entity_id).unwrap();
                    return (entity_type.entity_method)(&mut *self, addr, entity_id, elt);
                }

                let elt = elt.read_simple::<DebugElementUndefined<0>>()?;
                warn!(%addr, "<- Entity method (unknown selected entity): msg#{} {:?} (request: {:?})", id - id::ENTITY_METHOD.first, elt.element, elt.request_id);
                return Ok(false);

            }
            id if id::ENTITY_PROPERTY.contains(id) => {
                let elt = elt.read_simple::<DebugElementUndefined<0>>()?;
                warn!(%addr, "<- Entity property: msg#{} {:?} (request: {:?})", id - id::ENTITY_PROPERTY.first, elt.element, elt.request_id);
                return Ok(false);
            }
            id => {
                let elt = elt.read_simple::<DebugElementUndefined<0>>()?;
                error!(%addr, "<- Element #{id} {:?} (request: {:?})", elt.element, elt.request_id);
                return Ok(false);
            }
        }

        Ok(true)

    }

    fn read_create_base_player<E>(&mut self, addr: SocketAddr, elt: ElementReader) -> io::Result<bool>
    where E: Entity + fmt::Debug,
    {

        use client::element::CreateBasePlayer;

        let cbp = elt.read_simple::<CreateBasePlayer<E>>()?;

        let dump_file = self.shared.dump_dir.join(format!("entity_{}.txt", cbp.element.entity_id));
        let mut dump_writer = File::create(&dump_file)?;
        write!(dump_writer, "{:#?}", cbp.element.entity_data)?;

        info!(%addr, "<- Create base player: ({}) {}", cbp.element.entity_id, dump_file.display());

        Ok(true)

    }

    fn read_entity_method<E>(&mut self, addr: SocketAddr, entity_id: u32, elt: ElementReader) -> io::Result<bool>
    where 
        E: Entity,
        E::ClientMethod: fmt::Debug,
    {
        use client::element::EntityMethod;
        let em = elt.read_simple::<EntityMethod<E::ClientMethod>>()?;
        info!(%addr, "<- Entity method: ({entity_id}) {:?}", em.element.inner);
        Ok(true)
    }

    fn read_base_entity_method<E>(&mut self, addr: SocketAddr, entity_id: u32, elt: ElementReader) -> io::Result<bool>
    where 
        E: Entity,
        E::BaseMethod: fmt::Debug,
    {
        use base::element::BaseEntityMethod;
        let em = elt.read_simple::<BaseEntityMethod<E::BaseMethod>>()?;
        info!(%addr, "-> Base entity method: ({entity_id}) {:?}", em.element.inner);
        Ok(true)
    }

}

/// Represent an entity type and its associated static functions.
#[derive(Debug)]
struct EntityType {
    create_base_player: fn(&mut BaseThread, SocketAddr, ElementReader) -> io::Result<bool>,
    entity_method: fn(&mut BaseThread, SocketAddr, u32, ElementReader) -> io::Result<bool>,
    base_entity_method: fn(&mut BaseThread, SocketAddr, u32, ElementReader) -> io::Result<bool>,
}

impl EntityType {

    const fn new<E>() -> Self
    where
        E: Entity + fmt::Debug,
        E::ClientMethod: fmt::Debug,
        E::BaseMethod: fmt::Debug,
    {
        Self {
            create_base_player: BaseThread::read_create_base_player::<E>,
            entity_method: BaseThread::read_entity_method::<E>,
            base_entity_method: BaseThread::read_base_entity_method::<E>,
        }
    }

}

const ENTITY_TYPES: &[EntityType] = &[
    EntityType::new::<gen::entity::Account>(),
    EntityType::new::<gen::entity::Avatar>(),
    EntityType::new::<gen::entity::ArenaInfo>(),
    EntityType::new::<gen::entity::ClientSelectableObject>(),
    EntityType::new::<gen::entity::HangarVehicle>(),
    EntityType::new::<gen::entity::Vehicle>(),
    EntityType::new::<gen::entity::AreaDestructibles>(),
    EntityType::new::<gen::entity::OfflineEntity>(),
    EntityType::new::<gen::entity::Flock>(),
    EntityType::new::<gen::entity::FlockExotic>(),
    EntityType::new::<gen::entity::Login>(),
];
