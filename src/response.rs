use std::sync::mpsc;
use std::io::{self, Cursor};
use solicit::http::Header;

struct ResponseHandler {
    resolve: mpsc::Sender<Response>,
    tx: mpsc::Sender<Vec<u8>>,
}

impl ResponseHandler {
    fn on_headers(&mut self, headers: &[Header]) {
        // Disambiguate between trailers and headers!
        let (tx, rx) = mpsc::channel();
        self.resolve.send(Response::new(rx)).unwrap();
    }
    fn on_data(&mut self, buf: &[u8]) {
        self.tx.send(buf.into()).unwrap();
    }
}

struct FutureResponse {
    rx: mpsc::Receiver<Response>,
}

impl FutureResponse {
    pub fn new() -> (FutureResponse, mpsc::Sender<Response>) {
        let (tx, rx) = mpsc::channel();
        let future = FutureResponse {
            rx: rx,
        };
        (future, tx)
    }
    pub fn response(self) -> Option<Response> {
        self.rx.recv().ok()
    }
}

struct Response {
    rx: mpsc::Receiver<Vec<u8>>,
    leftover: Option<Cursor<Vec<u8>>>,
}

impl Response {
    fn new(rx: mpsc::Receiver<Vec<u8>>) -> Response {
        Response {
            rx: rx,
            leftover: None,
        }
    }
    fn get_next_chunk(&mut self) -> Cursor<Vec<u8>> {
        match self.rx.recv() {
            Ok(buf) => Cursor::new(buf),
            Err(_) => panic!("Foo"),
        }
    }
}

impl io::Read for Response {
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
