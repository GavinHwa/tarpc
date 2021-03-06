// Copyright 2016 Google Inc. All Rights Reserved.
//
// Licensed under the MIT License, <LICENSE or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed except according to those terms.

use byteorder::{BigEndian, ReadBytesExt};
use bytes::{MutBuf, Take};
use futures::Async;
use std::io;
use std::mem;
use tokio_proto::TryRead;
use tokio_proto::pipeline::Frame;
use util::Never;

#[derive(Debug)]
pub struct U64Reader {
    read: usize,
    data: [u8; 8],
}

impl U64Reader {
    fn new() -> Self {
        U64Reader {
            read: 0,
            data: [0; 8],
        }
    }
}

impl MutBuf for U64Reader {
    fn remaining(&self) -> usize {
        8 - self.read
    }

    unsafe fn advance(&mut self, count: usize) {
        self.read += count;
    }

    unsafe fn mut_bytes(&mut self) -> &mut [u8] {
        &mut self.data[self.read..]
    }
}

#[derive(Debug)]
enum NextReadAction<R> {
    Continue,
    Stop(Result<R, io::Error>),
}

trait MutBufExt: MutBuf + Sized {
    type Inner;

    fn take(&mut self) -> Self::Inner;

    fn try_read<R: TryRead>(&mut self, stream: &mut R) -> io::Result<NextReadAction<Self::Inner>> {
        while let Async::Ready(bytes_read) = stream.try_read_buf(self)? {
            debug!("Reader: read {} bytes, {} remaining.",
                   bytes_read,
                   self.remaining());
            if bytes_read == 0 {
                debug!("Reader: connection broken.");
                let err = io::Error::new(io::ErrorKind::BrokenPipe, "The connection was closed.");
                return Ok(NextReadAction::Stop(Err(err)));
            }

            if !self.has_remaining() {
                trace!("Reader: finished.");
                return Ok(NextReadAction::Stop(Ok(self.take())));
            }
        }
        Ok(NextReadAction::Continue)
    }
}

impl MutBufExt for U64Reader {
    type Inner = u64;

    fn take(&mut self) -> u64 {
        (&self.data as &[u8]).read_u64::<BigEndian>().unwrap()
    }
}

impl MutBufExt for Take<Vec<u8>> {
    type Inner = Vec<u8>;

    fn take(&mut self) -> Vec<u8> {
        mem::replace(self.get_mut(), vec![])
    }
}

/// A state machine that reads packets in non-blocking fashion.
#[derive(Debug)]
pub enum ReadState {
    /// Tracks how many bytes of the message size have been read.
    Len(U64Reader),
    /// Tracks read progress.
    Data(Take<Vec<u8>>),
}

#[derive(Debug)]
enum NextReadState {
    Same,
    Next(ReadState),
    Reset(Vec<u8>),
}

impl ReadState {
    pub fn init() -> ReadState {
        ReadState::Len(U64Reader::new())
    }

    pub fn next<R: TryRead>(&mut self,
                            socket: &mut R)
                            -> io::Result<Async<Frame<Vec<u8>, Never, io::Error>>> {
        loop {
            let next = match *self {
                ReadState::Len(ref mut len) => {
                    match len.try_read(socket)? {
                        NextReadAction::Continue => NextReadState::Same,
                        NextReadAction::Stop(result) => {
                            match result {
                                Ok(len) => {
                                    let buf = Vec::with_capacity(len as usize);
                                    NextReadState::Next(ReadState::Data(Take::new(buf,
                                                                                  len as usize)))
                                }
                                Err(e) => return Ok(Async::Ready(Frame::Error(e))),
                            }
                        }
                    }
                }
                ReadState::Data(ref mut buf) => {
                    match buf.try_read(socket)? {
                        NextReadAction::Continue => NextReadState::Same,
                        NextReadAction::Stop(result) => {
                            match result {
                                Ok(buf) => NextReadState::Reset(buf),
                                Err(e) => return Ok(Async::Ready(Frame::Error(e))),
                            }
                        }
                    }
                }
            };
            match next {
                NextReadState::Same => return Ok(Async::NotReady),
                NextReadState::Next(next) => *self = next,
                NextReadState::Reset(packet) => {
                    *self = ReadState::init();
                    return Ok(Async::Ready(Frame::Message(packet)));
                }
            }
        }
    }
}
