extern crate futures;
extern crate futures_state_stream;
#[macro_use]
extern crate serde_derive;
extern crate toml;
extern crate tokio_core;
extern crate tokio_imap;
extern crate tokio_postgres;

use futures::future::{Future, ok};
use futures::stream::{self, Stream};
use futures_state_stream::StateStream;
use std::env;
use std::fs::File;
use std::io::{self, Read};
use std::str;
use tokio_core::reactor::Core;
use tokio_imap::proto::{Attribute, CommandBuilder, FetchBuilderMessages, FetchBuilderAttributes, FetchBuilderModifiers};
use tokio_postgres::{Connection, TlsMode};

#[derive(Deserialize)]
struct Config {
    imap: ImapConfig,
    store: StoreConfig,
}

#[derive(Deserialize)]
struct ImapConfig {
    server: String,
    account: String,
    password: String,
}

#[derive(Deserialize)]
struct StoreConfig {
    uri: String,
}

struct Context {
    client: tokio_imap::Client,
    conn: Connection,
}

type ContextFuture = Future<Item = Context, Error = std::io::Error>;

fn check_folder(ctx: Context, label: Label) -> Box<ContextFuture> {
    let Context { client, conn } = ctx;
    Box::new(client.call(CommandBuilder::select(&label.name))
        .collect()
        .and_then(|(_, client)| {
            let cmd = CommandBuilder::fetch()
                .all_after(1)
                .attr(Attribute::Envelope)
                .changed_since(29248804)
                .build();
            client.call(cmd).for_each(|rd| {
                println!("server: {:?}", &rd.parsed());
                Ok(())
            })
        }).and_then(|client| ok(Context { client, conn }))
    )
}

struct Label {
    id: i32,
    name: String,
    mod_seq: i64,
}

fn sync_folders(ctx: Context) -> Box<Future<Item = Context, Error = io::Error>> {
    let Context { client, conn } = ctx;
    Box::new(conn.prepare("SELECT id, name, mod_seq FROM labels")
        .and_then(|(stmt, conn)| {
            conn.query(&stmt, &[]).map(|row| {
                Ok(Label {
                    id: row.get(0),
                    name: row.get(1),
                    mod_seq: row.get(2),
                })
            }).collect()
        })
        .map_err(|_| io::Error::new(io::ErrorKind::Other, "database error"))
        .and_then(|(labels, conn)| {
            let ctx = Context { client, conn };
            stream::iter(labels).fold(ctx, check_folder)
        })
        .and_then(ok))
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut f = File::open(&args[1]).unwrap();
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();
    let config: Config = toml::from_str(&s).unwrap();

    let mut core = Core::new().unwrap();
    let handle = core.handle();
    core.run(
        tokio_imap::Client::connect(&config.imap.server, &handle)
            .and_then(|(client, _)|
                client.call(CommandBuilder::login(
                    &config.imap.account, &config.imap.password))
                .collect())
            .join(
                Connection::connect(config.store.uri.clone(), TlsMode::None, &handle)
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e)))
            .and_then(|((_, client), conn)|
                sync_folders(Context { client, conn }))
    ).unwrap();
}
