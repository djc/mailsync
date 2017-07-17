extern crate futures;
#[macro_use]
extern crate serde_derive;
extern crate toml;
extern crate tokio_core;
extern crate tokio_imap;

use futures::future::{Future, ok};
use std::env;
use std::fs::File;
use std::io::Read;
use std::str;
use tokio_core::reactor::Core;
use tokio_imap::proto::{Attribute, CommandBuilder, FetchBuilderModifiers};

#[derive(Deserialize)]
struct Config {
    server: String,
    account: String,
    password: String,
}

fn show_responses(responses: tokio_imap::client::ServerMessages) {
    for rsp in responses.iter() {
        println!("server: {:?}", rsp);
    }
}

type ClientFuture = Future<Item = tokio_imap::Client, Error = std::io::Error>;

fn check_folder(client: tokio_imap::Client, name: &str) -> Box<ClientFuture> {
    Box::new(client.call(CommandBuilder::select(name))
        .and_then(|(client, responses)| {
             show_responses(responses);
             ok(client)
        }).and_then(|client| {
            let cmd = CommandBuilder::fetch()
                .all_after(1)
                .attr(Attribute::Envelope)
                .unwrap()
                .changed_since(29248804)
                .build();
            client.call(cmd).and_then(|(client, responses)| {
                show_responses(responses);
                ok(client)
            })
        })
    )
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut f = File::open(&args[1]).unwrap();
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();
    let config: Config = toml::from_str(&s).unwrap();
    let mut core = Core::new().unwrap();
    let handle = core.handle();
    let res = tokio_imap::Client::connect(&config.server, &handle).and_then(|(client, server_greeting)| {
        println!("greeting: {:?}", server_greeting.parsed());
        let cmd = CommandBuilder::login(&config.account, &config.password);
        client.call(cmd).and_then(|(client, responses)| {
            show_responses(responses);
            ok(client)
        }).and_then(|client| check_folder(client, "Inbox")
        ).and_then(|_| ok(()))
    });
    core.run(res).unwrap();
}
