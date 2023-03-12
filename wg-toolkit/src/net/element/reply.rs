//! Reply elements, for builtin support of request/replies.
//! These structures are used in `Bundle` structure and sub structures.

use std::io::{self, Read, Write};

use crate::util::io::*;

use super::{Element, SimpleElement, TopElement, ElementLength};


/// The element id for reply.
pub const REPLY_ID: u8 = 0xFF;


/// The element only decodes the request ID. This is used internally by bundle readers.
#[derive(Debug)]
pub struct ReplyHeader {
    /// The request ID this reply is for.
    pub request_id: u32,
}

impl SimpleElement for ReplyHeader {

    fn encode<W: Write>(&self, mut write: W) -> io::Result<()> {
        write.write_u32(self.request_id)
    }

    fn decode<R: Read>(mut read: R, len: usize) -> io::Result<Self> {
        Ok(ReplyHeader { request_id: read.read_u32()? })
    }

}

impl TopElement for ReplyHeader {
    const LEN: ElementLength = ElementLength::Variable32;
}


/// A wrapper for a reply element, with the request ID and the underlying element.
#[derive(Debug)]
pub struct Reply<E: Element> {
    /// The request ID this reply is for.
    pub request_id: u32,
    /// The inner reply element.
    pub element: E
}

impl<E: Element> Reply<E> {

    #[inline]
    pub fn new(request_id: u32, element: E) -> Self {
        Self { request_id, element }
    }
    
}

impl<E: Element> Element for Reply<E> {

    type Config = E::Config;

    fn encode<W: Write>(&self, mut write: W, config: &Self::Config) -> io::Result<()> {
        write.write_u32(self.request_id)?;
        self.element.encode(write, config)
    }

    fn decode<R: Read>(read: R, len: usize, config: &Self::Config) -> io::Result<Self> {
        Ok(Reply {
            request_id: read.read_u32()?,
            element: E::decode(read, len - 4, config)?,
        })
    }

}

impl<E: Element> TopElement for Reply<E> {
    const LEN: ElementLength = ElementLength::Variable32;
}
