use std::env;
use std::path::PathBuf;
use std::str;

use chrono::DateTime;
use email_parser::Message;
use mbox_reader;
use postgres::{Connection, TlsMode};

fn main() {
    let mut args = env::args();
    let name = PathBuf::from(args.nth(1).unwrap());
    let mbox = mbox_reader::MboxFile::from_file(&name).unwrap();
    let conn =
        Connection::connect("postgres://postgres@localhost:5432/mail-djc", TlsMode::None).unwrap();
    process(mbox, conn);
}

fn process(mbox: mbox_reader::MboxFile, conn: Connection) {
    let mut i = 0;
    let stmt = conn
        .prepare("INSERT INTO messages (dt, subject, mid, raw) VALUES ($1, $2, $3, $4)")
        .unwrap();
    for entry in mbox.iter() {
        if i % 1000 == 0 {
            println!("seen {}", i);
        }
        i += 1;

        // Strip GMail-specific "headers"
        let bytes = entry.message().unwrap();
        let mstr = str::from_utf8(bytes).unwrap();
        let mut split = mstr.splitn(2, "\r\n");
        let thrid = split.next();

        match thrid {
            Some(tid) => {
                if !tid.starts_with("X-GM-THRID:") {
                    println!("unexpected first header: {:?}", tid);
                }
            }
            None => panic!("first header not found: {:?}", bytes),
        }

        let foo = split.next();
        let mut rest = foo.unwrap();
        if rest.starts_with("X-Gmail-Labels:") {
            let mut split = rest.splitn(2, "\r\n");
            let labels = split.next();
            match labels {
                Some(lbls) => {
                    if !lbls.starts_with("X-Gmail-Labels:") {
                        println!("unexpected second header: {:?}", lbls);
                    }
                }
                None => panic!("second header not found: {:?}", bytes),
            }
            rest = split.next().unwrap();
        }

        let bytes = rest.as_bytes();
        let msg = Message::from_slice(bytes);
        let headers = msg.headers();
        let start = entry.start();
        let dt = DateTime::parse_from_str(start.date(), "%a %b %e %T %z %Y").unwrap();

        let mid_raw = headers.get_first("message-id");
        let message_id = match mid_raw {
            Some(ref mid) => Some(mid.as_ref().trim()),
            None => None as Option<&str>,
        };

        let subject_raw = headers.get_first("subject");
        let subject = match subject_raw {
            Some(ref subj) => Some(subj.as_ref().replace('\x00', "")),
            None => None as Option<String>,
        };

        let text = str::from_utf8(&bytes).unwrap();
        let res = if i == 251781 {
            let mut vec = bytes.to_vec();
            vec[6368] = b' ';
            let s = str::from_utf8(&vec).unwrap();
            stmt.execute(&[&dt, &subject, &message_id, &s])
        } else {
            stmt.execute(&[&dt, &subject, &message_id, &text])
        };
        match res {
            Err(e) => {
                println!("error for {}: {}", i, e);
            }
            _ => {}
        }
    }
    println!("DONE {}", i);
}
