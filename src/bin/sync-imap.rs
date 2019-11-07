use std::env;
use std::fs;

use email_parser::Message;
use futures::future::FutureExt;
use tokio_imap::client::builder::{CommandBuilder, FetchBuilderMessages, FetchBuilderModifiers};
use tokio_postgres::NoTls;
use toml;

use mailsync::{Config, ResponseAccumulator};

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    let s = fs::read_to_string(&args[1]).unwrap();
    let config: Config = toml::from_str(&s).unwrap();

    let (db, connection) = tokio_postgres::connect(&config.store.uri, NoTls)
        .await
        .unwrap();
    tokio::spawn(connection.map(|res| res.unwrap()));

    let (_, mut client) = tokio_imap::TlsClient::connect(&config.imap.server)
        .await
        .unwrap();
    let _ = client
        .call(CommandBuilder::login(
            &config.imap.account,
            &config.imap.password,
        ))
        .try_collect()
        .await
        .unwrap();

    let seen_seq: u32 = db
        .query_one("SELECT MAX(unid) FROM messages", &[])
        .await
        .unwrap()
        .get(0);

    let _ = client
        .call(CommandBuilder::examine("[Gmail]/All Mail"))
        .try_collect()
        .await
        .unwrap();

    let istmt = db
        .prepare(
            "INSERT INTO messages (unid, mod_seq, dt, subject, mid, bytes, flags) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .await
        .unwrap();

    eprintln!("Starting from UID {}...", seen_seq + 1);
    let cmd = CommandBuilder::uid_fetch().all_after((seen_seq + 1) as u32);
    let cmd = ResponseAccumulator::build_command_attributes(cmd);
    client
        .call(cmd.build())
        .try_fold((db, ResponseAccumulator::new()), |(db, acc), rd| {
            async {
                let (new, meta_opt) = acc.push(rd);
                if let Some(meta) = meta_opt {
                    let msg = Message::from_slice(&meta.raw);
                    let headers = msg.headers();
                    eprintln!("Storing message from {} (UID {})", meta.dt, meta.uid);

                    db.execute(
                        &istmt,
                        &[
                            &(meta.uid as i64),
                            &(meta.mod_seq as i64),
                            &meta.dt,
                            &headers.get_first("subject").map(|s| s.to_string()),
                            &headers.get_first("message-id").map(|s| s.to_string()),
                            &meta.raw,
                            &meta.flags,
                        ],
                    )
                    .await
                    .unwrap();
                }

                Ok((db, new))
            }
        })
        .await
        .unwrap();

    let _ = client
        .call(CommandBuilder::close())
        .try_collect()
        .await
        .unwrap();
}
