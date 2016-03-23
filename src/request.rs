use std::io::{self, Cursor};
use std::sync::mpsc;

use evtloop::{ConnectionHandle, Message};
use h2::{Stream, Http2, HttpMsg};

pub struct FreshRequest {
    conn: ConnectionHandle<Http2>,
}

// This struct could buffer some writes and send them to the stream once it
// becomes available...
pub struct FutureRequest {
    conn: ConnectionHandle<Http2>,
    future_stream: mpsc::Receiver<Stream>,
}

impl FutureRequest {
    pub fn await(self) -> Option<Request> {
        let conn = self.conn;
        self.future_stream.recv().ok().map(|stream|
            Request {
                conn: conn,
                stream: stream,
            }
        )
    }
}

pub struct Request {
    conn: ConnectionHandle<Http2>,
    stream: Stream,
}

impl Request {
    pub fn new(conn: ConnectionHandle<Http2>, stream: Stream) -> Request {
        Request {
            conn: conn,
            stream: stream,
        }
    }

    pub fn send(mut self) -> StreamResponse {
        io::Write::write(&mut self, b"Trailer! Done here!").unwrap();
        StreamResponse {
            _conn: self.conn,
            stream: self.stream,
            leftover: None,
        }
    }
}

impl io::Write for Request {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf);
        // TODO This could end up sending a lot of trywrite messages. An improvement
        //      would be to have a flag that indicates whether the connection is
        //      notified that it should try writing already. If so, then don't
        //      notify again. (The flag would have to be atomic with proper release
        //      acquire fences.)
        self.conn.notify(Message::TryWrite);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        // TODO Might want to consider having a flush-like message sent to the
        //      evtloop.
        Ok(())
    }
}

pub struct StreamResponse {
    _conn: ConnectionHandle<Http2>,
    stream: Stream,
    leftover: Option<Cursor<Vec<u8>>>,
}

impl StreamResponse {
    fn get_next_chunk(&mut self) -> Cursor<Vec<u8>> {
        match self.stream.recv_chunk() {
            Ok(buf) => Cursor::new(buf),
            Err(_) => panic!("Foo"),
        }
    }
}

impl io::Read for StreamResponse {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut cursor = match self.leftover.take() {
            Some(cursor) => cursor,
            None => self.get_next_chunk(),
        };
        let res = cursor.read(buf);
        if cursor.position() as usize == cursor.get_ref().len() {
            self.leftover = Some(cursor);
        }

        res
    }
}
