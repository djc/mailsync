use askama::Template;
use async_trait::async_trait;
use chrono::{DateTime, FixedOffset, Utc};
use err_derive::Error;
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE};
use hyper::Body;
use mailsync::Flag;
use mendes::http::{request::Parts, StatusCode};
use mendes::{dispatch, handler, types, Application, ClientError, Context};
use serde::{Deserialize, Serialize};

#[tokio::main]
async fn main() {
    mendes::hyper::run(&"[::]:3000".parse().unwrap(), App {})
        .await
        .unwrap();
}

#[handler(App)]
async fn ui(app: &App, _: &Parts) -> Result<Response, Error> {
    let db = sled::open("mail.sled").unwrap();
    let meta = db.open_tree("meta").unwrap();
    let messages = meta
        .iter()
        .values()
        .rev()
        .take(100)
        .map(|val| val.map(|v| bincode::deserialize(&v).unwrap()))
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    app.templated(Mailbox { messages })
}

#[derive(Template)]
#[template(path = "index.html")]
struct Mailbox {
    messages: Vec<MessageMeta>,
}

#[derive(Deserialize, Serialize)]
struct MessageMeta {
    seq: u32,
    uid: u32,
    mod_seq: u64,
    flags: Vec<Flag>,
    mid: Option<String>,
    dt: Option<DateTime<FixedOffset>>,
    subject: Option<String>,
    sender: Option<String>,
}

impl MessageMeta {
    fn unread(&self) -> bool {
        !self.flags.contains(&Flag::Seen)
    }

    fn sender_name(&self) -> &str {
        match &self.sender {
            Some(s) => {
                if s.trim().len() == 0 {
                    "(no sender)"
                } else if let Some(i) = s.find('<') {
                    &s[..i].trim()
                } else {
                    s
                }
            }
            None => "(no sender)",
        }
    }

    fn date(&self) -> String {
        match &self.dt {
            Some(dt) => {
                let dt = dt.with_timezone(&Utc);
                let now = Utc::now();
                let diff = now - dt;
                if diff.num_hours() < 24 {
                    format!("{}", dt.format("%H:%M"))
                } else if diff.num_weeks() < 26 {
                    format!("{}", dt.format("%b %e"))
                } else {
                    format!("{}", dt.format("%F"))
                }
            },
            None => "(no date)".into(),
        }
    }

    fn subject(&self) -> &str {
        let mut s: &str = match self.subject.as_ref() {
            Some(s) => s,
            None => return "(no subject)",
        };

        if s.starts_with("Re: ") {
            s = &s[4..];
        }

        s
    }
}

struct App {}

impl App {
    fn templated<T: Template>(&self, t: T) -> Result<Response, Error> {
        let content = t.render()?;
        Ok(hyper::Response::builder()
            .header(CONTENT_TYPE, types::HTML)
            .header(CONTENT_LENGTH, content.len())
            .body(content.into())?)
    }
}

#[async_trait]
impl Application for App {
    type RequestBody = Body;
    type ResponseBody = Body;
    type Error = Error;

    #[dispatch]
    async fn handle(mut cx: Context<Self>) -> Response {
        path! {
            _ => ui,
        }
    }

    fn error(&self, _: Error) -> Response {
        hyper::Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body("ERROR".into())
            .unwrap()
    }
}

#[derive(Debug, Error)]
enum Error {
    #[error(display = "client error: {:?}", _0)]
    Client(#[source] ClientError),
    #[error(display = "http error: {:?}", _0)]
    Http(#[source] mendes::http::Error),
    #[error(display = "template error: {:?}", _0)]
    Template(#[source] askama::Error),
}

type Response = mendes::http::Response<Body>;
