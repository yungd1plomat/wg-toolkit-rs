//! Package file codec.
//! 
//! Packages are ZIP files with constrained flags and properties,
//! for example no encryption and no compression is needed.
//! 
//! Following official specification: 
//! https://pkware.cachefly.net/webdocs/casestudies/APPNOTE.TXT

use std::fmt;
use std::io::{self, Seek, Read, SeekFrom, BufReader};

use crate::util::io::WgReadExt;


/// Signature for the Local File Header structure.
#[allow(unused)]
const LOCAL_FILE_HEADER_SIGNATURE: u32 = 0x04034b50;

/// Signature for the Central Directory Header structure.
const CENTRAL_DIRECTORY_HEADER_SIGNATURE: u32 = 0x02014b50;

/// Signature for the end of central directory.
const END_OF_CENTRAL_DIRECTORY_SIGNATURE: u32 = 0x06054b50;


/// A package-specialized ZIP reader that is optimized for reading all file names as fast
/// as possible. This reader only accesses file immutably.
pub struct PackageReader<R> {
    /// Underlying reader. Not buffered because once the header has been parsed, the data
    /// reading will be spread way over the default 8 KB block of the buffered reader,
    /// so this is useless.
    inner: R,
    /// This string buffer holds all file names, where subsequent file names can be 
    /// optimized
    name_buffer: String,
    /// All informations about each file available to the reader.
    file_infos: Vec<PackageFileInfo>,
}

/// Internal metadata about a file.
#[derive(Debug)]
struct PackageFileInfo {
    /// Offset of the package name into the global name buffer.
    name_offset: u32,
    /// Length of the file name in the global name buffer.
    name_len: u16,
    /// Offset within the file of the local header of this file.
    header_offset: u32,
}

impl<R: Read + Seek> PackageReader<R> {

    /// Create a package reader with the underlying read+seek implementor.
    pub fn new(reader: R) -> io::Result<Self> {

        // Here we need to parse the "End of Central Directory".

        const HEADER_MIN_SIZE: u64 = 22;
        const HEADER_MAX_SIZE: u64 = 22 + u16::MAX as u64;

        // For decoding the package structure we use a buffered reader to optimize
        // our random reads.
        let mut reader = BufReader::new(reader);

        // Here we try to find the position of the End of Central Directory.
        let file_length = reader.seek(SeekFrom::End(0))?;
        let mut eocd_pos = file_length.checked_sub(HEADER_MIN_SIZE)
            .ok_or(io::Error::from(io::ErrorKind::InvalidData))?;
        let eocd_pos_bound = file_length.saturating_sub(HEADER_MAX_SIZE);

        // A successful return from this loop means we found the EoCD position.
        loop {

            reader.seek(SeekFrom::Start(eocd_pos))?;
            if reader.read_u32()? == END_OF_CENTRAL_DIRECTORY_SIGNATURE {
                break;
            }

            if eocd_pos == eocd_pos_bound {
                // If we didn't find signature on the lower bound.
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }

            eocd_pos = eocd_pos.checked_sub(1)
                .ok_or(io::Error::from(io::ErrorKind::InvalidData))?;

        }

        // Here we finish parsing the EoCD (we are placed just after the directory signature).
        let disk_number = reader.read_u16()?;
        let disk_with_central_directory = reader.read_u16()?;

        if disk_number != disk_with_central_directory {
            // Multi-disk ZIP files are not valid packages.
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }

        let number_of_files_on_this_disk = reader.read_u16()?;
        let number_of_files = reader.read_u16()?;

        if number_of_files_on_this_disk != number_of_files {
            // Same as above, no multi-disk, so the number of files must be coherent.
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }

        let _central_directory_size = reader.read_u32()?;
        let central_directory_offset = reader.read_u32()?;

        let comment_length = reader.read_u16()?;
        if comment_length != 0 {
            // Not expecting comments on packages.
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }

        // Now we can start parsing all Central Directory Headers.
        // Seek to the first Central Directory Header, reading is ready.
        reader.seek(SeekFrom::Start(central_directory_offset as u64))?;

        // At start, we only read file names and optimize their storage, the actual file
        // header, size, flags will be read only when the file is accessed, here we only
        // read file name and store the offset header.
        // On average in World of Tanks packages, there is 70 bytes per file name.
        let mut name_buffer = Vec::with_capacity(number_of_files as usize * 70);
        let mut file_infos = Vec::with_capacity(number_of_files as usize);

        for _ in 0..number_of_files {

            if reader.read_u32()? != CENTRAL_DIRECTORY_HEADER_SIGNATURE {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }

            // Skip most of the header that we don't care at this point.
            reader.seek_relative(24)?;
            // Then we read all variable lengths.
            let file_name_len = reader.read_u16()?;
            // Read both fields at once because we want ot check that it's zero.
            let extra_field_file_comment_len = reader.read_u32()?;
            // Skip again, disk num, file attrs.
            reader.seek_relative(8)?;
            // Then read the offset of the local file header.
            let relative_offset = reader.read_u32()?;

            // Extra field and comment are not supported nor used by Wargaming.
            if extra_field_file_comment_len != 0 {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
            
            // Start by increasing the buffer capacity.
            let name_offset = name_buffer.len() as u32;  // FIXME: Checked cast
            name_buffer.resize(name_buffer.len() + file_name_len as usize, 0);
            reader.read_exact(&mut name_buffer[name_offset as usize..][..file_name_len as usize])?;
            
            // Push the metadata to the files array.
            file_infos.push(PackageFileInfo {
                name_offset,
                name_len: file_name_len,
                header_offset: relative_offset,
            });

        }
        
        let name_buffer = String::from_utf8(name_buffer).unwrap();

        Ok(Self { 
            inner: reader.into_inner(), 
            name_buffer,
            file_infos,
        })

    }

    /// Return the number of files in the package.
    #[inline]
    pub fn len(&self) -> usize {
        self.file_infos.len()
    }
    
    /// Return an iterator over all file names in the package. The position of file names
    /// in this iterator is the same that can be used when reading from index, using
    /// the [`Self::read_by_index()`] method.
    pub fn names(&self) -> impl Iterator<Item = &'_ str> + '_ {
        self.file_infos.iter().map(|file| {
            &self.name_buffer[file.name_offset as usize..][..file.name_len as usize]
        })
    }

    // Find a file index from its name.
    pub fn index_by_name(&self, file_name: &str) -> Option<usize> {
        self.names().position(|check| check == file_name)
    }

    /// Open a package file by its name.
    pub fn read_by_name(&mut self, file_name: &str) -> io::Result<PackageFileReader<'_, R>> {
        // FIXME: For now it's a brute force, but later we could make a string map.
        let file_index = self.index_by_name(file_name)
            .ok_or(io::Error::from(io::ErrorKind::NotFound))?;
        self.read_by_index(file_index)
    }

    /// Internal function to open a package from its metadata.
    /// 
    /// Note that the returned reader has no buffered over the original reader given at
    /// construction, you should handle buffering if necessary.
    pub fn read_by_index(&mut self, file_index: usize) -> io::Result<PackageFileReader<'_, R>> {

        let info = self.file_infos.get(file_index)
            .ok_or(io::Error::from(io::ErrorKind::NotFound))?;

        // Start to the start of the header.
        self.inner.seek(SeekFrom::Start(info.header_offset as u64))?;
        if self.inner.read_u32()? != LOCAL_FILE_HEADER_SIGNATURE {
            return Err(io::ErrorKind::InvalidData.into());
        }

        // Skip version needed to extract.
        self.inner.seek(SeekFrom::Current(2))?;
        let flags = self.inner.read_u16()?;
        let compression_method = self.inner.read_u16()?;
        // Skip file time/date/crc32
        self.inner.seek(SeekFrom::Current(2 + 2 + 4))?;
        let compressed_size = self.inner.read_u32()?;
        let uncompressed_size = self.inner.read_u32()?;
        // Skip file name len + extra field length because it has already been checked.
        self.inner.seek(SeekFrom::Current(4 + info.name_len as i64))?;

        // Packages has no flag, no delayed crc32/size, no compression, no encryption.
        if flags != 0 {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }

        // Packages don't compress files.
        if compression_method != 0 || compressed_size != uncompressed_size {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        
        // Now the reader's cursor is at data start, return the file reader.
        Ok(PackageFileReader {
            inner: &mut self.inner,
            initial_len: compressed_size,
            remaining_len: compressed_size,
        })

    }

}


/// A handle for reading a file in a package.
#[derive(Debug)]
pub struct PackageFileReader<'a, R> {
    /// Underlying reader.
    inner: &'a mut R,
    /// Full length of this file.
    initial_len: u32,
    /// Remaining length to read from the file.
    remaining_len: u32,
}

impl<R: Read + Seek> Read for PackageFileReader<'_, R> {

    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // If remaining length is zero, this will just do nothing.
        let len = buf.len().min(self.remaining_len as usize);
        let len = self.inner.read(&mut buf[..len])?;
        self.remaining_len -= len as u32;
        Ok(len)
    }

    #[inline]
    fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        if (self.remaining_len as usize) < buf.len() {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.inner.read_exact(buf)?;
        self.remaining_len -= buf.len() as u32;
        Ok(())
    }

}

impl<R: Read + Seek> Seek for PackageFileReader<'_, R> {

    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {

        // Calculate the past length that has been read so far.
        let position = self.initial_len - self.remaining_len;

        let delta = match pos {
            SeekFrom::Start(offset) => {

                if (self.initial_len as u64) < offset {
                    return Err(io::ErrorKind::InvalidInput.into());
                }

                -(position as i64) + offset as i64

            }
            SeekFrom::End(offset) => {
                
                if offset > 0 || offset < -(self.initial_len as i64) {
                    return Err(io::ErrorKind::InvalidInput.into());
                }

                (self.remaining_len as i64) + offset

            }
            SeekFrom::Current(offset) => {

                // If we go forward but we don't have enough data.
                if offset > 0 && (self.remaining_len as i64) < offset {
                    return Err(io::ErrorKind::InvalidInput.into());
                } else if offset < 0 && (position as i64) < -offset {
                    return Err(io::ErrorKind::InvalidInput.into());
                }
                
                offset

            }
        };

        self.inner.seek(SeekFrom::Current(delta))?;
        self.remaining_len = (self.remaining_len as i64 - delta) as u32;
        Ok((self.initial_len - self.remaining_len) as u64)

    }

    #[inline]
    fn stream_position(&mut self) -> io::Result<u64> {
        Ok((self.initial_len - self.remaining_len) as u64)
    }

}

impl<R: fmt::Debug> fmt::Debug for PackageReader<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PackageReader")
            .field("inner", &self.inner)
            .field("name_buffer", &self.name_buffer.len())
            .field("file_infos", &self.file_infos.len()).finish()
    }
}
