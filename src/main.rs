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
use tokio_imap::proto::{AttrMacro, CommandBuilder};

#[derive(Deserialize)]
struct Config {
    server: String,
    account: String,
    password: String,
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
        println!("server: {:?}", server_greeting);
        let cmd = CommandBuilder::login(&config.account, &config.password);
        client.call(cmd).and_then(|(client, responses)| {
            for rsp in responses.iter() {
                println!("server: {:?}", rsp);
            }
            ok(client)
        }).and_then(|client| {
            let cmd = CommandBuilder::select("Inbox");
            client.call(cmd).and_then(|(client, responses)| {
                for rsp in responses.iter() {
                    println!("server: {:?}", rsp);
                }
                ok(client)
            })
        }).and_then(|client| {
            let cmd = CommandBuilder::fetch()
                .all_after(1)
                .attr_macro(AttrMacro::Fast)
                .unwrap()
                .changed_since(29248804)
                .build();
            client.call(cmd).and_then(|(_, responses)| {
                for rsp in responses.iter() {
                    println!("server: {:?}", rsp);
                }
                ok(())
            })
        })
    });
    core.run(res).unwrap();
}
