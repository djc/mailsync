extern crate futures;
#[macro_use]
extern crate serde_derive;
extern crate toml;
extern crate tokio_core;
extern crate tokio_imap;
extern crate tokio_postgres;

use futures::future::{Future, ok};
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

fn check_folder(ctx: Context, name: &str) -> Box<ContextFuture> {
    let Context { client, conn } = ctx;
    Box::new(client.call(CommandBuilder::select(name))
        .and_then(|(client, _)| {
            let cmd = CommandBuilder::fetch()
                .all_after(1)
                .attr(Attribute::Envelope)
                .changed_since(29248804)
                .build();
            client.call(cmd).and_then(|(client, responses)| {
                for rsp in responses.iter() {
                    println!("server: {:?}", &rsp);
                }
                ok(Context { client, conn })
            })
        })
    )
}

fn sync_folders(ctx: Context) -> Box<Future<Item = (), Error = io::Error>> {
    Box::new(check_folder(ctx, "Inbox").and_then(|_| ok(())))
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
            .and_then(|(client, _)| client.call(CommandBuilder::login(
                &config.imap.account, &config.imap.password))
            .join(
                Connection::connect(config.store.uri.clone(), TlsMode::None, &handle)
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e))))
            .and_then(|((client, _), conn)|
                sync_folders(Context { client, conn: conn }))
    ).unwrap();
}
