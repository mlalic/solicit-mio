//! A very simple Future implementation backed by `mpsc` channels.

use std::sync::mpsc;

struct Future<T> {
    rx: mpsc::Receiver<T>,
}

impl<T> Future<T> {
    fn from_channel(rx: mpsc::Receiver<T>) -> Future<T> {
        Future {
            rx: rx,
        }
    }

    fn wait(self) -> Option<T> {
        self.rx.recv().ok()
    }
}

struct ChainFuture<T, U> {
    future: 
}
