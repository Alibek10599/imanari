mod common;

use common::*;
use imanari::http;
use imanari::launch::{Session, TIMEOUT, call};
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

const CLIENT_ID: &str = "fieldwork-test";

#[derive(Clone, Copy, PartialEq)]
enum Tamper {
    None,
    Audience,
    Issuer,
    Expired,
    Nonce,
    Signature,
}

struct Grant {
    nonce: String,
    challenge: String,
    redirect_uri: String,
    subject: String,
}

struct Idp {
    issuer: String,
    keys: Vec<RsaKeyPair>,
    active: usize,
    subject: String,
    tamper: Tamper,
    codes: HashMap<String, Grant>,
}

fn b64url(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

fn decode(s: &str) -> String {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap()
}

fn form(s: &str) -> HashMap<String, String> {
    s.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (decode(k), decode(v)))
        .collect()
}

fn key(dir: &Path, name: &str) -> RsaKeyPair {
    let path = dir.join(format!("{name}.der"));
    let out = Command::new("openssl")
        .args([
            "genpkey",
            "-algorithm",
            "RSA",
            "-pkeyopt",
            "rsa_keygen_bits:2048",
        ])
        .args(["-outform", "DER", "-out", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    RsaKeyPair::from_pkcs8(&std::fs::read(path).unwrap()).unwrap()
}

impl Idp {
    fn jwks(&self) -> Value {
        let k: ring::rsa::PublicKeyComponents<Vec<u8>> = self.keys[self.active].public().into();
        json!({ "keys": [{
            "kty": "RSA",
            "use": "sig",
            "alg": "RS256",
            "kid": format!("k{}", self.active),
            "n": b64url(&k.n),
            "e": b64url(&k.e),
        }]})
    }

    fn id_token(&self, g: &Grant) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let t = self.tamper;
        let claims = json!({
            "iss": if t == Tamper::Issuer { "http://127.0.0.1:1" } else { self.issuer.as_str() },
            "sub": g.subject,
            "aud": if t == Tamper::Audience { "someone-else" } else { CLIENT_ID },
            "iat": now - if t == Tamper::Expired { 7200 } else { 0 },
            "exp": now + if t == Tamper::Expired { -3600 } else { 300 },
            "nonce": if t == Tamper::Nonce { "wrong-nonce" } else { g.nonce.as_str() },
            "email": format!("{}@example.test", g.subject),
        });
        let header = json!({ "alg": "RS256", "typ": "JWT", "kid": format!("k{}", self.active) });
        let input = format!(
            "{}.{}",
            b64url(header.to_string().as_bytes()),
            b64url(claims.to_string().as_bytes())
        );
        let signer = &self.keys[if t == Tamper::Signature {
            2
        } else {
            self.active
        }];
        let mut sig = vec![0; signer.public().modulus_len()];
        signer
            .sign(
                &RSA_PKCS1_SHA256,
                &ring::rand::SystemRandom::new(),
                input.as_bytes(),
                &mut sig,
            )
            .unwrap();
        format!("{input}.{}", b64url(&sig))
    }

    fn handle(
        &mut self,
        method: &str,
        target: &str,
        body: &str,
    ) -> (u16, Vec<(String, String)>, String) {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let json_ok = |v: Value| {
            (
                200,
                vec![("Content-Type".into(), "application/json".into())],
                v.to_string(),
            )
        };
        match (method, path) {
            ("GET", "/.well-known/openid-configuration") => json_ok(json!({
                "issuer": self.issuer,
                "authorization_endpoint": format!("{}/authorize", self.issuer),
                "token_endpoint": format!("{}/token", self.issuer),
                "jwks_uri": format!("{}/jwks", self.issuer),
                "response_types_supported": ["code"],
                "subject_types_supported": ["public"],
                "id_token_signing_alg_values_supported": ["RS256"],
            })),
            ("GET", "/jwks") => json_ok(self.jwks()),
            ("GET", "/authorize") => {
                let q = form(query);
                assert_eq!(q["client_id"], CLIENT_ID);
                assert_eq!(q["response_type"], "code");
                assert_eq!(q["code_challenge_method"], "S256");
                assert!(q["scope"].split(' ').any(|s| s == "openid"));
                let code = imanari::random_hex(16);
                self.codes.insert(
                    code.clone(),
                    Grant {
                        nonce: q["nonce"].clone(),
                        challenge: q["code_challenge"].clone(),
                        redirect_uri: q["redirect_uri"].clone(),
                        subject: self.subject.clone(),
                    },
                );
                let location = format!("{}?code={code}&state={}", q["redirect_uri"], q["state"]);
                (302, vec![("Location".into(), location)], String::new())
            }
            ("POST", "/token") => {
                let f = form(body);
                let grant = self
                    .codes
                    .remove(f.get("code").map(String::as_str).unwrap_or_default());
                let valid = grant.as_ref().is_some_and(|g| {
                    f.get("grant_type").map(String::as_str) == Some("authorization_code")
                        && f.get("redirect_uri") == Some(&g.redirect_uri)
                        && f.get("code_verifier")
                            .is_some_and(|v| b64url(&Sha256::digest(v.as_bytes())) == g.challenge)
                });
                match grant.filter(|_| valid) {
                    Some(g) => json_ok(json!({
                        "access_token": "opaque",
                        "token_type": "Bearer",
                        "expires_in": 300,
                        "id_token": self.id_token(&g),
                    })),
                    None => (
                        400,
                        vec![("Content-Type".into(), "application/json".into())],
                        json!({ "error": "invalid_grant" }).to_string(),
                    ),
                }
            }
            _ => (404, Vec::new(), String::new()),
        }
    }
}

fn serve(idp: Arc<Mutex<Idp>>, listener: TcpListener) {
    std::thread::spawn(move || {
        for stream in listener.incoming().map_while(Result::ok) {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let mut parts = line.split_whitespace();
            let (method, target) = (
                parts.next().unwrap_or_default().to_string(),
                parts.next().unwrap_or_default().to_string(),
            );
            let mut len = 0;
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).is_err() || h.trim().is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':')
                    && k.eq_ignore_ascii_case("content-length")
                {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; len];
            let _ = reader.read_exact(&mut body);
            let (status, headers, text) =
                idp.lock()
                    .unwrap()
                    .handle(&method, &target, &String::from_utf8_lossy(&body));
            let mut resp = format!(
                "HTTP/1.1 {status} X\r\nConnection: close\r\nContent-Length: {}\r\n",
                text.len()
            );
            for (k, v) in headers {
                resp.push_str(&format!("{k}: {v}\r\n"));
            }
            resp.push_str("\r\n");
            resp.push_str(&text);
            let mut stream = reader.into_inner();
            let _ = stream.write_all(resp.as_bytes());
        }
    });
}

fn path_of(url: &str, port: u16) -> String {
    url.strip_prefix(&format!("http://127.0.0.1:{port}"))
        .unwrap_or_else(|| panic!("{url} is not on port {port}"))
        .to_string()
}

fn cookie_of<'a>(r: &'a http::Response, name: &str) -> Option<&'a str> {
    r.headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
        .map(|(_, v)| v.as_str())
        .find(|v| v.starts_with(&format!("{name}=")))
}

fn login(app: u16, idp: u16) -> Result<Session, u16> {
    let start = http::request(app, "GET", "/_nari/login", &[], b"", TIMEOUT).unwrap();
    assert_eq!(start.status, 302, "{:?}", start.json());
    let state_cookie = cookie_of(&start, "nari_login").unwrap();
    assert!(state_cookie.contains("HttpOnly") && state_cookie.contains("SameSite=Lax"));
    let state = state_cookie.split(';').next().unwrap().to_string();
    let authorize = path_of(start.header("location").unwrap(), idp);
    let back = http::request(idp, "GET", &authorize, &[], b"", TIMEOUT).unwrap();
    let callback = path_of(back.header("location").unwrap(), app);
    let done = http::request(app, "GET", &callback, &[("Cookie", &state)], b"", TIMEOUT).unwrap();
    if done.status != 302 {
        assert!(cookie_of(&done, "nari_session").is_none());
        return Err(done.status);
    }
    let session = cookie_of(&done, "nari_session").unwrap();
    assert!(session.contains("HttpOnly") && session.contains("SameSite=Strict"));
    assert!(
        !session.contains("Secure"),
        "plain-http public URLs get no Secure flag"
    );
    let cookie = session.split(';').next().unwrap().to_string();
    let me = http::request(
        app,
        "GET",
        "/_nari/session",
        &[("Cookie", &cookie)],
        b"",
        TIMEOUT,
    )
    .unwrap()
    .json();
    Ok(Session {
        cookie,
        csrf: me["data"]["user"]["csrf"].as_str().unwrap().to_string(),
        actor: me["data"]["user"]["id"].as_str().unwrap().to_string(),
    })
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn oidc_sign_in_verifies_tokens_and_scopes_data_per_subject() {
    let dir = scratch("oidc-idp");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let idp_port = listener.local_addr().unwrap().port();
    let issuer = format!("http://127.0.0.1:{idp_port}");
    let idp = Arc::new(Mutex::new(Idp {
        issuer: issuer.clone(),
        keys: vec![key(&dir, "k0"), key(&dir, "k1"), key(&dir, "rogue")],
        active: 0,
        subject: "alice".into(),
        tamper: Tamper::None,
        codes: HashMap::new(),
    }));
    serve(idp.clone(), listener);

    let src = FIXTURE.replace(
        "    mode = local_dev;",
        &format!("    mode = oidc;\n    issuer = \"{issuer}\";\n    client_id = \"{CLIENT_ID}\";"),
    );
    let b = build("oidc", &src);
    let port = free_port();
    let public = format!("http://127.0.0.1:{port}");
    let app = start(
        &b,
        &dir.join("app.db"),
        &[
            ("NARI_PORT", &port.to_string()),
            ("NARI_PUBLIC_URL", &public),
        ],
    )
    .expect("app starts after OIDC discovery");
    assert!(
        app.bootstrap.is_empty(),
        "no development bootstrap in oidc mode"
    );
    for path in ["/_nari/bootstrap", "/_nari/dev/select"] {
        let r = http::request(port, "POST", path, &[("Origin", &public)], b"{}", TIMEOUT).unwrap();
        assert_eq!(r.status, 404, "{path} is not served in oidc mode");
    }

    let alice = login(port, idp_port).expect("alice signs in");
    assert_eq!(alice.actor, imanari_runtime::oidc_actor(&issuer, "alice"));
    let r = call(
        port,
        Some(&alice),
        "create_task",
        &json!({ "title": "alice's" }),
        Some("o1"),
    )
    .unwrap();
    assert_eq!(r.status, 200, "{:?}", r.json());

    idp.lock().unwrap().subject = "bob".into();
    let bob = login(port, idp_port).expect("bob signs in");
    assert_ne!(bob.actor, alice.actor);
    let list = |s: &Session| {
        call(port, Some(s), "list_tasks", &json!({}), None)
            .unwrap()
            .json()["data"]["items"]
            .as_array()
            .unwrap()
            .len()
    };
    assert_eq!(list(&bob), 0, "bob cannot see alice's task");

    idp.lock().unwrap().subject = "alice".into();
    let again = login(port, idp_port).unwrap();
    assert_eq!(
        again.actor, alice.actor,
        "the same subject maps to the same actor"
    );
    assert_eq!(list(&again), 1);

    for tamper in [
        Tamper::Audience,
        Tamper::Issuer,
        Tamper::Expired,
        Tamper::Nonce,
        Tamper::Signature,
    ] {
        idp.lock().unwrap().tamper = tamper;
        assert_eq!(login(port, idp_port).err(), Some(401));
    }
    idp.lock().unwrap().tamper = Tamper::None;

    let start_login = http::request(port, "GET", "/_nari/login", &[], b"", TIMEOUT).unwrap();
    let authorize = path_of(start_login.header("location").unwrap(), idp_port);
    let back = http::request(idp_port, "GET", &authorize, &[], b"", TIMEOUT).unwrap();
    let callback = path_of(back.header("location").unwrap(), port);
    let forged = http::request(
        port,
        "GET",
        &callback,
        &[("Cookie", "nari_login=forged")],
        b"",
        TIMEOUT,
    )
    .unwrap();
    assert_eq!(
        forged.status, 401,
        "a callback from another browser is refused"
    );
    let state = cookie_of(&start_login, "nari_login")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let ok = http::request(port, "GET", &callback, &[("Cookie", &state)], b"", TIMEOUT).unwrap();
    assert_eq!(ok.status, 302);
    let replay =
        http::request(port, "GET", &callback, &[("Cookie", &state)], b"", TIMEOUT).unwrap();
    assert_eq!(replay.status, 401, "a callback cannot be replayed");

    idp.lock().unwrap().active = 1;
    let rotated = login(port, idp_port).expect("a rotated signing key is picked up");
    assert_eq!(rotated.actor, alice.actor);

    let out = http::request(
        port,
        "POST",
        "/_nari/logout",
        &[
            ("Cookie", &rotated.cookie),
            ("X-Nari-CSRF", &rotated.csrf),
            ("Origin", &public),
        ],
        b"{}",
        TIMEOUT,
    )
    .unwrap();
    assert_eq!(out.status, 200);
    let after = call(port, Some(&rotated), "list_tasks", &json!({}), None).unwrap();
    assert_eq!(after.status, 401, "a signed-out session is gone");

    let https = start(
        &b,
        &dir.join("app2.db"),
        &[("NARI_PUBLIC_URL", "https://tasks.example.test")],
    )
    .unwrap();
    let r = http::request(
        https.port,
        "GET",
        "/_nari/login",
        &[("Host", "tasks.example.test")],
        b"",
        TIMEOUT,
    )
    .unwrap();
    assert_eq!(r.status, 302, "the public host is accepted");
    assert!(cookie_of(&r, "nari_login").unwrap().contains("; Secure"));
    assert!(
        r.header("location")
            .unwrap()
            .contains("redirect_uri=https%3A%2F%2Ftasks.example.test%2F_nari%2Fcallback")
    );
}
