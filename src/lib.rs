extern crate futures;
extern crate futures_state_stream;
#[macro_use]
extern crate serde_derive;
extern crate toml;
extern crate tokio_core;
extern crate tokio_imap;
extern crate tokio_postgres;

use futures::future::Future;

use std::io;

use tokio_postgres::Connection;


#[derive(Deserialize)]
pub struct Config {
    pub imap: ImapConfig,
    pub store: StoreConfig,
}

#[derive(Deserialize)]
pub struct ImapConfig {
    pub server: String,
    pub account: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct StoreConfig {
    pub uri: String,
}

pub type ContextFuture = Future<Item = Context, Error = SyncError>;

pub struct Context {
    pub client: tokio_imap::Client,
    pub conn: Connection,
}

#[derive(Debug)]
pub struct Label {
    pub id: i32,
    pub name: String,
    pub mod_seq: Option<i64>,
}

macro_rules! error_enum_impls {
    ($name:ident, $( $variant:ident : $ty:path ),+ ) => {
        $(impl From<$ty> for $name {
            fn from(e: $ty) -> Self {
                $name:: $variant (e)
            }
        })*
    };
}

macro_rules! error_enum {
    ($name:ident, $( $variant:ident : $ty:path ),+ $(,)* ) => {
        #[derive(Debug)]
        pub enum $name {
            $($variant($ty)),*
        }
        error_enum_impls!($name, $($variant : $ty),*);
    };
}

error_enum!(SyncError,
    Io: io::Error,
    Pg: tokio_postgres::error::Error,
    PgConnect: tokio_postgres::error::ConnectError,
);
