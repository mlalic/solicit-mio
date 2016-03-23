#![feature(drain)]

#[macro_use] extern crate log;
extern crate mio;
extern crate solicit;

pub mod evtloop;
pub mod request;
// pub mod h2o;
pub mod h2;
pub mod simple;

pub use request::Request;
