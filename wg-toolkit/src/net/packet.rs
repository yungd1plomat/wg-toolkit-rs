//! Packet structure definition with synchronization methods.

use std::io::{Cursor, Read, Write, Seek};
use std::fmt::{Debug, Formatter};
use std::collections::VecDeque;

use byteorder::{ReadBytesExt, WriteBytesExt, LE};

// use crate::util::BytesFmt;


/// According to disassembly of WoT, outside of a channel, the max size if always
/// `1500 - 28 = 1472`, this includes the 4-bytes prefix.
pub const PACKET_MAX_LEN: usize = 1472;
/// The length of the unknown 4-byte prefix.
pub const PACKET_PREFIX_LEN: usize = 4;
/// Flags are u16.
pub const PACKET_FLAGS_LEN: usize = 2;
/// Minimum length of a raw packet.
pub const PACKET_MIN_LEN: usize = PACKET_PREFIX_LEN + PACKET_FLAGS_LEN;

/// Maximum size that can possibly taken by the footer.
/// - 8 for sequence range
/// - 4 for first request offset
/// - 4 for sequence number
/// - 1 for single acks count
/// - 4 * 1 for at least one single acks
/// - 4 for cumulative ack
/// - 8 for indexed channel (not yet supported in sync data/state)
/// - 4 for checksum
pub const PACKET_MAX_FOOTER_LEN: usize = 8 + 4 + 4 + 1 + 4 + 4 /*+ 8*/ + 4;

// /// The theoretical maximum length for the body, if maximum length is used by header + footer.
// pub const PACKET_MAX_BODY_LEN: usize =
//     PACKET_MAX_LEN -
//     PACKET_MAX_FOOTER_LEN -
//     PACKET_FLAGS_LEN -
//     PACKET_PREFIX_LEN;

// /// The offset of the 16 bit flags in the raw data of a packet.
// pub const PACKET_FLAGS_OFFSET: usize = PACKET_PREFIX_LEN;
// /// The offset of the packet's body in the raw data of a packet.
// pub const PACKET_BODY_OFFSET: usize = PACKET_PREFIX_LEN + PACKET_FLAGS_LEN;


/// Raw packet layout with only data and length. This structure provides functions for
/// growing and shrinking data, retrieving and modifying its length. Other states such
/// are footer offset or first request offset are not saved in this structure, because
/// this structure is intended to be used as backend of the [`Packet`] structure which
/// contains such state.
/// 
/// The internal data is split in multiple slices that are accessible through the API:
/// 
/// - *Raw data*, it contains the full internal data with max data length, this should
///   be used for receiving datagram from the network;
/// 
/// - *Data*, it contains all the data up to the packet's length;
/// 
/// - *Body data*, it contains all the data starting with the packet's flags up to the
///   packet's length.
/// 
#[derive(Clone, Debug)]
pub struct RawPacket {
    /// Full raw data of the packet.
    data: [u8; PACKET_MAX_LEN],
    /// Length of the packet, must not be lower than minimum length which
    /// contains the prefix and the flags.
    len: usize,
}

impl RawPacket {

    #[inline]
    pub fn new() -> Self {
        Self { 
            data: [0; PACKET_MAX_LEN], 
            len: PACKET_MIN_LEN,
        }
    }

    /// Get a slice to the full raw data, this means that this isn't 
    /// constrained by the length of the packet.
    #[inline]
    pub fn raw_data(&self) -> &[u8] {
        &self.data[..]
    }

    /// Get a mutable slice to the full raw data, this means that this isn't 
    /// constrained by the length of the packet.
    /// 
    /// This mutable slice can be used to receive data from an UDP datagram.
    #[inline]
    pub fn raw_data_mut(&mut self) -> &mut [u8] {
        &mut self.data[..]
    }

    /// Return the maximum size of a packet.
    #[inline]
    pub fn max_len(&self) -> usize {
        self.data.len()
    }

    /// Return the length of this packet.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Set the length of this packet. The function panics if the length
    /// is not at least `PACKET_MIN_LEN` or at most `PACKET_MAX_LEN`.
    #[inline]
    pub fn set_len(&mut self, len: usize) {
        assert!(len >= PACKET_MIN_LEN, "given length too small");
        assert!(len <= PACKET_MAX_LEN, "given length too high");
        self.len = len;
    }

    /// Return the available length in this packet.
    #[inline]
    pub fn available_len(&self) -> usize {
        self.max_len() - self.len()
    }

    /// Get a slice to the data, with the packet's length.
    /// 
    /// This slice can be used to send data as an UDP datagram for exemple.
    #[inline]
    pub fn data(&self) -> &[u8] {
        &self.data[..self.len]
    }

    /// Get a mutable slice to the data, with the packet's length.
    #[inline]
    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data[..self.len]
    }

    /// Return the maximum size of the body of a packet.
    #[inline]
    pub fn max_body_len(&self) -> usize {
        self.max_len() - PACKET_PREFIX_LEN
    }

    /// Return the length of this packet.
    #[inline]
    pub fn body_len(&self) -> usize {
        self.len() - PACKET_PREFIX_LEN
    }

    /// Get a slice to the data from after the prefix to the end.
    #[inline]
    pub fn body_data(&self) -> &[u8] {
        &self.data[PACKET_PREFIX_LEN..self.len]
    }

    /// Get a mutable slice to the data from after the prefix to the end.
    #[inline]
    pub fn body_data_mut(&mut self) -> &[u8] {
        &mut self.data[PACKET_PREFIX_LEN..self.len]
    }

    /// Reset this packet's length, flags and prefix.
    #[inline]
    pub fn reset(&self) {
        self.len = PACKET_MIN_LEN;
        self.data[..PACKET_MIN_LEN].fill(0);
    }

    /// Grow the packet's data by a given amount of bytes, and return a
    /// mutable slice to the newly allocated data.
    /// 
    /// This function panics if the available length is smaller than
    /// requested length.
    #[inline]
    pub fn grow(&mut self, len: usize) -> &mut [u8] {
        assert!(self.available_len() >= len, "not enough available data");
        let ptr = &mut self.data[self.len..][..len];
        self.len += len;
        ptr
    }

    /// Grow the packet's data by a given amount of bytes, and return
    /// a writer to the given data. This writer can be used to write
    /// new data to the newly allocated data.
    /// 
    /// This function panics if the available length is smaller than
    /// requested length.
    #[inline]
    pub fn grow_write(&mut self, len: usize) -> impl Write + Seek + '_ {
        Cursor::new(self.grow(len))
    }

    /// Shrink the packet's data by a given amount of bytes, and return
    /// a slice to the deallocated data. The slice is not mutable because
    /// returned data is no longer contained in packet's data.
    /// 
    /// The discarded data is left untouched, which mean that you can 
    /// rollback to the previous length to recover the data.
    /// 
    /// This function panics if the length after shrink is lower than
    /// prefix (4 bytes) + flags (2) bytes.
    #[inline]
    pub fn shrink(&mut self, len: usize) -> &[u8] {
        assert!(self.len - len >= PACKET_MIN_LEN, "not enough data to shrink");
        self.len -= len;
        &self.data[self.len..][..len]
    }

    /// Shrink the packet's data by a given amount of bytes, and return
    /// a reader to the freed data.
    /// 
    /// This function panics if the length after shrink is lower than
    /// prefix (4 bytes) + flags (2) bytes.
    #[inline]
    pub fn shrink_read(&mut self, len: usize) -> impl Read + '_ {
        Cursor::new(self.shrink(len))
    }

    /// Read the prefix of this packet. 
    #[inline]
    pub fn read_prefix(&self) -> u32 {
        u32::from_le_bytes(self.data[..PACKET_PREFIX_LEN].try_into().unwrap())
    }

    /// Write the prefix of this packet.
    #[inline]
    pub fn write_prefix(&mut self, prefix: u32) {
        self.data[..PACKET_PREFIX_LEN].copy_from_slice(&prefix.to_le_bytes())
    }

    /// Read the flags of this packet.
    #[inline]
    pub fn read_flags(&self) -> u16 {
        u16::from_le_bytes(self.data[PACKET_PREFIX_LEN..][..PACKET_FLAGS_LEN].try_into().unwrap())
    }

    /// Write the flags of this packet.
    #[inline]
    pub fn write_flags(&mut self, flags: u16) {
        self.data[PACKET_PREFIX_LEN..][..PACKET_FLAGS_LEN].copy_from_slice(&flags.to_le_bytes())
    }

}


/// Represent a [`RawPacket`] with additional state. The additional state keeps
/// track of different offsets in the packet's raw data. Like footer and first
/// request element offsets. This structure also provides functions for
/// synchronizing data from the state and vice-versa.
#[derive(Clone)]
pub struct Packet {
    /// The internal raw packet used for data manipulation.
    raw: RawPacket,
    /// Offset of the footer when the packet is finalized or loaded. The footer
    /// size if the difference between the raw packet's length and this footer
    /// offset.
    footer_offset: usize,
    /// The offset of the first element (see bundle) that is also a request in 
    /// the packet. If there are more requests in the packet, their offset is
    /// written in a link manner in the N-1 element.
    first_request_offset: usize,
}

impl Packet {

    /// Create a new packet instance.
    #[inline]
    pub fn new() -> Self {
        Self {
            raw: RawPacket::new(),
            footer_offset: PACKET_MIN_LEN,
            first_request_offset: 0,
        }
    }

    /// Create a new packet instance on the heap and returns the box containing it.
    pub fn new_boxed() -> Box<Self> {
        Box::new(Self::new())
    }

    /// Return a shared reference to the internal raw packet.
    #[inline]
    pub fn raw(&self) -> &RawPacket {
        &self.raw
    }

    /// Return a mutable reference to the internal raw packet.
    /// **You should** be really careful when manipulating the internal data.
    #[inline]
    pub fn raw_mut(&mut self) -> &mut RawPacket {
        &mut self.raw
    }

    /// Return the maximum content length.
    #[inline]
    pub fn max_len(&self) -> usize {
        // Subtract length of prefix + flags + max footer.
        self.raw.max_len() - PACKET_MIN_LEN - PACKET_MAX_FOOTER_LEN
    }

    /// Return the length of the content.
    #[inline]
    pub fn len(&self) -> usize {
        self.footer_offset - PACKET_MIN_LEN
    }

    /// Return the available body length for writing elements. The rest of the
    /// length might be used for the footer.
    #[inline]
    pub fn available_len(&self) -> usize {
        self.max_len() - self.len()
    }

    #[inline]
    pub fn data(&self) -> &[u8] {
        &self.raw.raw_data()[PACKET_MIN_LEN..self.footer_offset]
    }

    #[inline]
    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.raw.raw_data()[PACKET_MIN_LEN..self.footer_offset]
    }

    #[inline]
    pub fn grow(&mut self, len: usize) -> &mut [u8] {
        assert!(self.available_len() >= len, "not enough available data");
        let ptr = &mut self.raw.raw_data()[self.footer_offset..][..len];
        self.footer_offset += len;
        // Reset len to footer offset because we could've override footers.
        self.raw.set_len(self.footer_offset);
        ptr
    }

    #[inline]
    pub fn grow_write(&mut self, len: usize) -> impl Write + '_ {
        Cursor::new(self.grow(len))
    }
    
    /// Return the length of the footer. It should not exceed `PACKET_MAX_FOOTER_LEN`.
    #[inline]
    pub fn footer_len(&self) -> usize {
        self.raw.len() - self.footer_offset
    }

    #[inline]
    pub fn footer_available_len(&self) -> usize {
        PACKET_MAX_FOOTER_LEN - self.footer_len()
    }

    /// Return the offset of the next request element in this packet. Because
    /// this offset cannot be equal to 0 or 1 (which points to packet's flags),
    /// such values are sentinels that fill returns `None`.
    #[inline]
    pub fn first_request_offset(&self) -> Option<usize> {
        (self.first_request_offset >= PACKET_FLAGS_LEN).then_some(self.first_request_offset)
    }

    /// Set the first offset of the next request element in this packet. Refer
    /// to `first_request_offset` function for limitations.
    #[inline]
    pub fn set_first_request_offset(&mut self, offset: usize) {
        assert!(offset >= PACKET_FLAGS_LEN, "invalid request offset");
        self.first_request_offset = offset;
    }

    /// Clear the first request offset.
    #[inline]
    pub fn clear_first_request_offset(&mut self) {
        self.first_request_offset = 0;
    }

    /// Synchronize internal packet's data from its state. This function takes a
    /// configuration that will be applied to the packet, the configuration must
    /// be mutable because the function will try to put the maximum number of
    /// acks in the footer, the remaining acks will be left over in the config.
    pub fn sync_data(&mut self, config: &mut PacketConfig) {

        // If the footer is already filled
        if self.footer_offset < self.raw.len() {
            self.raw.set_len(self.footer_offset);
        }

        // Note that in this function we are intentionally using the function 
        // 'self.raw.grow[_write]'. This will cause the raw length to grow 
        // without the footer offset, which will increase the footer length.

        let mut flags = 0u16;

        if config.reliable() { flags |= flags::IS_RELIABLE; }
        if config.on_channel() { flags |= flags::ON_CHANNEL; }
        
        if let Some((first_num, last_num)) = config.sequence_range() {
            flags |= flags::IS_FRAGMENT;
            let mut cursor = self.raw.grow_write(8);
            cursor.write_u32::<LE>(first_num).unwrap();
            cursor.write_u32::<LE>(last_num).unwrap();
        }

        if let Some(request_offset) = self.first_request_offset() {
            flags |= flags::HAS_REQUESTS;
            self.raw.grow_write(2).write_u16::<LE>(request_offset as u16).unwrap();
        }

        // TODO: The 0x1000 flag's value go here.

        if config.reliable() || config.sequence_range().is_some() {
            flags |= flags::HAS_SEQUENCE_NUMBER;
            self.raw.grow_write(4).write_u32::<LE>(config.sequence_num()).unwrap();
        }

        if !config.single_acks().is_empty() {

            flags |= flags::HAS_ACKS;

            // Compute the remaining footer length for acks.
            // TODO: Add indexed channel bytes count when supported.
            let mut available_len = self.footer_available_len()
                - if config.cumulative_ack().is_some() { 4 } else { 0 }
                - if config.has_checksum() { 4 } else { 0 }
                - 1; // Acks count

            let mut count = 0;
            while let Some(ack) = config.single_acks_mut().pop_front() {
                if available_len < 4 {
                    break
                } else {
                    self.raw.grow_write(4).write_u32::<LE>(ack).unwrap();
                    count += 1;
                }
            }

            debug_assert!(count != 0);
            self.raw.grow(1)[0] = count as _;

        }

        if let Some(num) = config.cumulative_ack() {
            flags |= flags::HAS_CUMULATIVE_ACK;
            self.raw.grow_write(4).write_u32::<LE>(num).unwrap();
        }

        // TODO: Indexed channel flag's value go here.

        if config.has_checksum() {
            flags |= flags::HAS_CHECKSUM;
        }

        // Finally, write flags just before computing checksum (if needed).
        self.raw.write_flags(flags);

        // If checksum enabled, compute the checksum of the whole body of the packet,
        // which range from flags to the end of the footer. The checksum will be
        // appended to the footer after computing the checksum.
        if config.has_checksum() {
            let checksum = calc_checksum(Cursor::new(self.raw.body_data()));
            self.raw.grow_write(4).write_u32::<LE>(checksum).unwrap();
        }

    }

    /// Synchronize internal packet's state from its raw data.
    /// 
    /// *Note that* the given length must account for the prefix.
    ///
    /// *If this function returns an error, the integrity of the internal state is not guaranteed.*
    pub fn sync_state(&mut self, len: usize, config: &mut PacketConfig) -> Result<(), PacketSyncError> {

        // We set the length of the raw packet, it allow us to use 
        // 'shrink_read' on it to read each footer element.
        self.raw.set_len(len);

        // Start by reading flags.
        let flags = self.raw.read_flags();

        // This list of flags contains all flags supported by this function.
        const KNOWN_FLAGS: u16 =
            flags::HAS_CHECKSUM |
            flags::HAS_CUMULATIVE_ACK |
            flags::HAS_ACKS |
            flags::HAS_SEQUENCE_NUMBER |
            flags::HAS_REQUESTS |
            flags::IS_FRAGMENT |
            flags::ON_CHANNEL |
            flags::IS_RELIABLE;

        if flags & !KNOWN_FLAGS != 0 {
            return Err(PacketSyncError::UnknownFlags(flags & !KNOWN_FLAGS));
        }

        if flags | flags::HAS_CHECKSUM != 0 {

            // We shrink the packet to read the checksum and then compute the checksum 
            // from the body data, which no longer contains the checksum itself!
            let expected_checksum = self.raw.shrink_read(4).read_u32::<LE>().unwrap();
            let computed_checksum = calc_checksum(Cursor::new(self.raw.body_data()));

            if expected_checksum != computed_checksum {
                return Err(PacketSyncError::InvalidChecksum)
            }

        }

        // TODO: Indexed channel flag's value go here.

        if flags | flags::HAS_CUMULATIVE_ACK != 0 {
            let ack = self.raw.shrink_read(4).read_u32::<LE>().unwrap();
            if ack == 0 {
                // Zero is a sentinel value that isn't valid.
                return Err(PacketSyncError::Corrupted)
            } else {
                config.set_cumulative_ack(ack);
            }
        }

        if flags | flags::HAS_ACKS != 0 {

            let count = self.raw.shrink(1)[0];
            if count == 0 {
                return Err(PacketSyncError::Corrupted)
            }

            for _ in 0..count {
                config.single_acks_mut().push_back(self.raw.shrink_read(4).read_u32::<LE>().unwrap());
            }

        }

        let mut has_sequence_num = false;
        if flags | flags::HAS_SEQUENCE_NUMBER != 0 {
            config.set_sequence_num(self.raw.shrink_read(4).read_u32::<LE>().unwrap());
            has_sequence_num = true;
        }

        // TODO: The 0x1000 flag's value go here.

        if flags | flags::HAS_REQUESTS != 0 {
            let offset = self.raw.shrink_read(2).read_u16::<LE>().unwrap() as usize;
            if offset < PACKET_FLAGS_LEN {
                return Err(PacketSyncError::Corrupted)
            } else {
                self.set_first_request_offset(offset);
            }
        }

        if flags | flags::IS_FRAGMENT != 0 {
            let mut cursor = self.raw.shrink_read(8);
            let first_num = cursor.read_u32::<LE>().unwrap();
            let last_num = cursor.read_u32::<LE>().unwrap();
            if first_num >= last_num {
                return Err(PacketSyncError::Corrupted)
            } else {
                config.set_sequence_range(first_num, last_num);
            }
        }

        config.set_reliable(flags | flags::IS_RELIABLE != 0);
        config.set_on_channel(flags | flags::ON_CHANNEL != 0);

        // Now that we shrunk all the footer, set the footer offset.
        self.footer_offset = self.raw.len();
        // Rollback the length.
        self.raw.set_len(len);

        // Check that the footer length is coherent.
        debug_assert!(self.footer_len() <= PACKET_MAX_FOOTER_LEN);

        Ok(())

    }

}

impl Debug for Packet {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {

        let mut s = f.debug_struct("Packet");

        s.field("len", &self.len());
        // s.field("raw_len", &self.net_len());
        // s.field("body_len", &self.body_len());
        
        // s.field("prefix", &format_args!("{:X}", BytesFmt(&self.raw_data()[..PACKET_PREFIX_LEN])));
        // s.field("body", &format_args!("{:X}", BytesFmt(self.body_data())));

        // if self.footer_offset < self.len {
        //     s.field("footer_offset", &self.footer_offset);
        //     s.field("footer_len", &(self.len - self.footer_offset));
        // }

        // if let Some(request_offset) = self.first_request_offset() {
        //     s.field("request_offset", &request_offset);
        // }

        // s.field("seq", &self.sequence_num());

        // if let Some((first_num, last_num)) = self.sequence_range() {
        //     s.field("seq_first", &first_num);
        //     s.field("seq_last", &last_num);
        // }

        s.finish()

    }
}


/// Describe a packet configuration that can be used when synchronizing data or 
/// state of a packet.
#[derive(Debug, Clone)]
pub struct PacketConfig {
    /// The sequence number of this packet, it is used if reliable mode is enabled
    /// **and/or** if the packet is a fragment of a chain of packet.
    sequence_num: u32,
    /// If this packet is a fragment (defined just after), this contains the
    /// sequence number of the first packet in the chain.
    /// 
    /// A packet is considered to be a fragment of a chain only if `seq_first < 
    /// seq_last`.
    sequence_first_num: u32,
    /// If this packet is a fragment (defined in `seq_first` doc), this contains 
    /// the sequence number of the last packet in the chain.
    sequence_last_num: u32,
    /// Set to true if the sender of this packet requires an acknowledgment from
    /// the receiver upon successful receipt of this packet.
    reliable: bool,
    /// The cumulative ack number. This number is sent for acknowledging that
    /// all sequence numbers up to (but excluding) this ack have been received.
    /// Because it is excluding, **it should not be equal to zero**.
    cumulative_ack: u32,
    /// Individual acks to send.
    single_acks: VecDeque<u32>,
    /// Set to true when this packet is being transferred on a channel.
    on_channel: bool,
    /// Enable or disable checksum.
    has_checksum: bool,
}

impl PacketConfig {

    /// Create a new packet configuration with default values.
    #[inline]
    pub fn new() -> Self {
        Self {
            sequence_num: 0,
            sequence_first_num: 0,
            sequence_last_num: 0,
            reliable: false,
            cumulative_ack: 0,
            single_acks: VecDeque::new(),
            on_channel: false,
            has_checksum: false,
        }
    }

    /// Returns the sequence number of this packet. It is actually used only if
    /// this packet is marked as reliable **and/or** if the packet is a fragment.
    /// 
    /// It is set to 0 by default.
    #[inline]
    pub fn sequence_num(&self) -> u32 {
        self.sequence_num
    }

    /// Set the sequence number of this packet. Read `sequence_num` doc for 
    /// explanation of the usage of the sequence number.
    #[inline]
    pub fn set_sequence_num(&mut self, num: u32) {
        self.sequence_num = num;
    }

    /// Returns the range of sequence number in case this packet is a fragment
    /// of a packet chain. Both bounds are included.
    #[inline]
    pub fn sequence_range(&self) -> Option<(u32, u32)> {
        if self.sequence_first_num < self.sequence_last_num {
            Some((self.sequence_first_num, self.sequence_last_num))
        } else {
            None
        }
    }

    /// Set the range of sequence number if this packet is a fragment of a
    /// packet chain. Both bounds are included and `last` should be greater
    /// than `first`, this function panics if this condition is not met.
    /// 
    /// See also `clear_sequence_range` if you want to clear the range.
    /// 
    /// *Note that* the sequence number is not checked to be in bounds.
    #[inline]
    pub fn set_sequence_range(&mut self, first: u32, last: u32) {
        assert!(first < last, "invalid range");
        self.sequence_first_num = first;
        self.sequence_last_num = last;
    }

    /// Clear the range of sequence number. After calling this, the packet 
    /// is no longer a fragment in a packet chain.
    #[inline]
    pub fn clear_sequence_range(&mut self) {
        self.sequence_first_num = 0;
        self.sequence_last_num = 0;
    }

    /// Returns true if the sender of this packet requires an acknowledgment from 
    /// the receiver upon successful receipt of this packet.
    #[inline]
    pub fn reliable(&self) -> bool {
        self.reliable
    }

    /// Read `reliable` doc for explanation of this value.
    #[inline]
    pub fn set_reliable(&mut self, reliable: bool) {
        self.reliable = reliable
    }

    /// This number is sent for acknowledging that all sequence numbers up to (but 
    /// excluding) this ack have been received.
    #[inline]
    pub fn cumulative_ack(&self) -> Option<u32> {
        (self.cumulative_ack != 0).then_some(self.cumulative_ack)
    }

    #[inline]
    pub fn single_acks(&self) -> &VecDeque<u32> {
        &self.single_acks
    }

    #[inline]
    pub fn single_acks_mut(&self) -> &mut VecDeque<u32> {
        &mut self.single_acks
    }

    /// Set the cumulative ack if this packet. Because this value is an excluded
    /// bound, you should not set this to 0. If you want to reset the cumulative
    /// ack, use `clear_cumulative_ack` instead.
    #[inline]
    pub fn set_cumulative_ack(&mut self, num: u32) {
        assert_ne!(num, 0, "ack number is zero");
        self.cumulative_ack = num;
    }

    /// Clear the cumulative ack from this packet.
    #[inline]
    pub fn clear_cumulative_ack(&mut self) {
        self.cumulative_ack = 0;
    }

    #[inline]
    pub fn on_channel(&self) -> bool {
        self.on_channel
    }

    #[inline]
    pub fn set_on_channel(&mut self, on_channel: bool) {
        self.on_channel = on_channel;
    }

    #[inline]
    pub fn has_checksum(&self) -> bool {
        self.has_checksum
    }

    #[inline]
    pub fn set_checksum(&mut self, enabled: bool) {
        self.has_checksum = enabled;
    }

}


/// Generic function to calculate the checksum from a reader and
/// a given number of bytes available.
fn calc_checksum(mut reader: impl Read) -> u32 {
    let mut checksum = 0;
    while let Ok(num) = reader.read_u32::<LE>() {
        checksum ^= num;
    }
    checksum
}


/// Internal module defining flags for packets.
#[allow(unused)]
mod flags {
    pub const HAS_REQUESTS: u16        = 0x0001;
    pub const HAS_PIGGYBACKS: u16      = 0x0002;
    pub const HAS_ACKS: u16            = 0x0004;
    pub const ON_CHANNEL: u16          = 0x0008;
    pub const IS_RELIABLE: u16         = 0x0010;
    pub const IS_FRAGMENT: u16         = 0x0020;
    pub const HAS_SEQUENCE_NUMBER: u16 = 0x0040;
    pub const INDEXED_CHANNEL: u16     = 0x0080;
    pub const HAS_CHECKSUM: u16        = 0x0100;
    pub const CREATE_CHANNEL: u16      = 0x0200;
    pub const HAS_CUMULATIVE_ACK: u16  = 0x0400;
}


/// Packet synchronization error.
#[derive(Debug)]
pub enum PacketSyncError {
    /// Unknown flags are used, the packet can't be decoded because this usually
    /// increase length of the footer.
    UnknownFlags(u16),
    /// The packet is corrupted, the footer might be too short or an invalid bit
    /// pattern has been read.
    Corrupted,
    /// The packet checksum and calculated checksum aren't equal.
    InvalidChecksum
}
