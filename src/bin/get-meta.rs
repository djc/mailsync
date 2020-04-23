use std::collections::HashMap;
use std::env;
use std::str;

use csv;
use futures::future::ok;
use serde_derive::{Deserialize, Serialize};
use tokio_imap::client::builder::{
    CommandBuilder, FetchBuilderAttributes, FetchBuilderMessages, FetchBuilderModifiers,
};
use tokio_imap::proto::ResponseData;
use tokio_imap::types::{Attribute, AttributeValue, MailboxDatum, Response};
use tokio_imap::TlsClient;

use mailsync::Config;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    let config = Config::from_file(&args[1]);

    let mut writer = csv::Writer::from_path("imap-meta.csv").unwrap();
    let (_, mut client) = TlsClient::connect(&config.imap.server).await.unwrap();
    let _ = client
        .call(CommandBuilder::login(
            &config.imap.account,
            &config.imap.password,
        ))
        .try_collect()
        .await
        .unwrap();

    let msgs = client
        .call(CommandBuilder::examine("[Gmail]/All Mail"))
        .try_collect()
        .await
        .unwrap();
    let exists = msgs
        .iter()
        .filter_map(|rd| match *rd.parsed() {
            Response::MailboxData(MailboxDatum::Exists(num)) => Some(num),
            _ => None,
        })
        .nth(0)
        .unwrap();

    let (start, end) = (1, exists);
    println!("fetch metadata for {}:{}", start, end);
    let cmd = CommandBuilder::fetch()
        .range(start, end)
        .attr(Attribute::Uid)
        .attr(Attribute::ModSeq)
        .attr(Attribute::Flags)
        .attr(Attribute::Envelope)
        .build();

    let _ = client
        .call(cmd)
        .try_fold(ResponseAccumulator::new(4), |acc, rd| {
            let (new, meta_opt) = acc.push(rd);
            if let Some(meta) = meta_opt {
                if meta.seq % 1000 == 0 {
                    println!("store metadata for index {}", meta.seq);
                }
                writer.serialize(meta).unwrap();
            }
            ok(new)
        })
        .await
        .unwrap();

    let _ = client
        .call(CommandBuilder::close())
        .try_collect()
        .await
        .unwrap();
}

struct ResponseAccumulator {
    parts: HashMap<u32, (u32, Vec<ResponseData>)>,
    num_parts: u32,
}

impl ResponseAccumulator {
    fn new(num_parts: u32) -> ResponseAccumulator {
        ResponseAccumulator {
            parts: HashMap::new(),
            num_parts,
        }
    }
    fn push(mut self, rd: ResponseData) -> (Self, Option<MessageMeta>) {
        use crate::AttributeValue::*;
        let completed = {
            let (idx, entry) = match *rd.parsed() {
                Response::Fetch(idx, ref attr_vals) => {
                    let mut entry = self.parts.entry(idx).or_insert((0, vec![]));
                    for val in attr_vals.iter() {
                        entry.0 += match *val {
                            Uid(_) | ModSeq(_) | Flags(_) | Envelope(_) => 1,
                            _ => 0,
                        };
                    }
                    (idx, entry)
                }
                _ => return (self, None),
            };
            entry.1.push(rd);
            if entry.0 == self.num_parts {
                let mut mod_seq = None;
                let mut uid = None;
                let mut mid = None;
                let mut dt = None;
                let mut subject = None;
                let mut sender = None;
                let mut flags = None;
                for rd in entry.1.drain(..) {
                    match *rd.parsed() {
                        Response::Fetch(_, ref attr_vals) => {
                            for val in attr_vals.iter() {
                                match *val {
                                    Uid(u) => {
                                        uid = Some(u);
                                    }
                                    ModSeq(ms) => {
                                        mod_seq = Some(ms);
                                    }
                                    Flags(ref fs) => {
                                        let list: Vec<&str> = fs.iter().map(|f| *f).collect();
                                        flags = Some(list.join(" "));
                                    }
                                    Envelope(ref env) => {
                                        mid = env
                                            .message_id
                                            .map(|r| String::from_utf8_lossy(r).into());
                                        dt = env.date.map(|r| String::from_utf8_lossy(r).into());
                                        subject =
                                            env.subject.map(|r| String::from_utf8_lossy(r).into());
                                        if let Some(ref senders) = env.sender {
                                            sender = Some(format!(
                                                "{} <{}@{}>",
                                                String::from_utf8_lossy(
                                                    senders[0].name.unwrap_or(b"")
                                                ),
                                                String::from_utf8_lossy(
                                                    senders[0].mailbox.unwrap_or(b"")
                                                ),
                                                String::from_utf8_lossy(
                                                    senders[0].host.unwrap_or(b"")
                                                ),
                                            ));
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    };
                }
                Some(MessageMeta {
                    seq: idx,
                    uid: uid.unwrap(),
                    mod_seq: mod_seq.unwrap(),
                    flags: flags.unwrap(),
                    mid,
                    date: dt,
                    subject,
                    sender,
                })
            } else {
                None
            }
        };
        if let Some(ref meta) = completed {
            self.parts.remove(&meta.seq);
        }
        (self, completed)
    }
}

#[derive(Deserialize, Serialize)]
struct MessageMeta {
    seq: u32,
    uid: u32,
    mod_seq: u64,
    flags: String,
    mid: Option<String>,
    date: Option<String>,
    subject: Option<String>,
    sender: Option<String>,
}
