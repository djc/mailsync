extern crate email_parser;
extern crate futures;
extern crate futures_state_stream;
extern crate mailsync;
extern crate tokio_core;
extern crate tokio_imap;
extern crate tokio_postgres;
extern crate toml;

use email_parser::Message;

use futures::future::{ok, Future};
use futures_state_stream::StateStream;

use mailsync::*;

use std::env;
use std::fs::File;
use std::io::Read;

use tokio_core::reactor::Core;

use tokio_imap::client::builder::*;
use tokio_imap::client::ImapClient;

use tokio_postgres::{Connection, TlsMode};

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut f = File::open(&args[1]).unwrap();
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();
    let config: Config = toml::from_str(&s).unwrap();

    let mut core = Core::new().unwrap();
    let handle = core.handle();
    core.run(
        tokio_imap::client::connect(&config.imap.server)
            .unwrap()
            .map_err(|e| SyncError::from(e))
            .and_then(|(client, _)| {
                client
                    .call(CommandBuilder::login(
                        &config.imap.account,
                        &config.imap.password,
                    ))
                    .collect()
                    .map_err(|e| SyncError::from(e))
            })
            .join(
                Connection::connect(config.store.uri.clone(), TlsMode::None, &handle)
                    .map_err(|e| SyncError::from(e)),
            )
            .and_then(|((_, client), conn)| sync_messages(Context { client, conn })),
    )
    .unwrap();
}

fn sync_messages(ctx: Context) -> Box<ContextFuture> {
    let Context { client, conn } = ctx;
    Box::new(
        conn.prepare("SELECT MAX(unid) FROM messages")
            .map_err(|e| SyncError::from(e))
            .and_then(|(stmt, conn)| {
                conn.query(&stmt, &[])
                    .map_err(|e| SyncError::from(e))
                    .collect()
            })
            .join(
                client
                    .call(CommandBuilder::examine("[Gmail]/All Mail"))
                    .collect()
                    .map_err(|e| SyncError::from(e)),
            )
            .and_then(|((rows, conn), (_, client))| {
                let seen_seq: i64 = rows[0].get(0);
                conn.prepare(
                    "INSERT INTO messages (unid, mod_seq, dt, subject, mid, bytes, flags) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7)",
                )
                .map_err(|e| SyncError::from(e))
                .and_then(move |(istmt, conn)| ok((seen_seq, istmt, conn, client)))
            })
            .and_then(move |(seen_seq, istmt, conn, client)| {
                eprintln!("Starting from UID {}...", seen_seq + 1);
                let cmd = CommandBuilder::uid_fetch().all_after((seen_seq + 1) as u32);
                let cmd = ResponseAccumulator::build_command_attributes(cmd);
                client
                    .call(cmd.build())
                    .map_err(|e| SyncError::from(e))
                    .fold(
                        (conn, ResponseAccumulator::new()),
                        move |(conn, acc), rd| {
                            let (new, meta_opt) = acc.push(rd);
                            if let Some(meta) = meta_opt {
                                let msg = Message::from_slice(&meta.raw);
                                let headers = msg.headers();
                                eprintln!("Storing message from {} (UID {})", meta.dt, meta.uid);
                                Box::new(
                                    conn.execute(
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
                                    .map_err(|e| {
                                        eprintln!("PG error: {:?}", e);
                                        SyncError::from(e)
                                    })
                                    .and_then(|(_, conn)| ok((conn, new))),
                                )
                                    as Box<
                                        Future<
                                            Item = (Connection, ResponseAccumulator),
                                            Error = SyncError,
                                        >,
                                    >
                            } else {
                                Box::new(ok::<(Connection, ResponseAccumulator), SyncError>((
                                    conn, new,
                                )))
                            }
                        },
                    )
            })
            .and_then(|((conn, _), client)| {
                client
                    .call(CommandBuilder::close())
                    .collect()
                    .map_err(|e| SyncError::from(e))
                    .join(ok(conn))
            })
            .and_then(|((_, client), conn)| ok(Context { client, conn }))
            .map_err(|e| SyncError::from(e)),
    )
}
