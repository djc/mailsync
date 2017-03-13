#[macro_use]
extern crate serde_derive;
extern crate toml;
extern crate tokio_imap;

use std::env;
use std::fs::File;
use std::io::Read;
use std::str;

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
    let client = tokio_imap::Client::connect(&config.server);
    client.login(&config.account, &config.password);
}
