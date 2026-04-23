use axum::{
    body::Body,
    extract::Request,
    http::{HeaderValue, header},
    middleware::Next,
    response::Response,
};
use uuid::Uuid;

#[derive(Clone, Copy)]
pub struct SessionId(pub Uuid);

pub async fn session_cookie(mut req: Request<Body>, next: Next) -> Response {
    let existing = req
        .headers()
        .get(header::COOKIE)
        .and_then(|h| h.to_str().ok())
        .and_then(find_session_cookie);

    let (session_id, is_new) = match existing {
        Some(Some(id)) => (id, false),
        Some(None) => (Uuid::new_v4(), false),
        None => (Uuid::new_v4(), true),
    };

    req.extensions_mut().insert(SessionId(session_id));
    let mut response = next.run(req).await;

    if is_new {
        let cookie = format!(
            "ione_session={}; Path=/; SameSite=Lax; Max-Age=31536000",
            session_id
        );
        if let Ok(value) = HeaderValue::from_str(&cookie) {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
    }

    response
}

fn find_session_cookie(cookie_header: &str) -> Option<Option<Uuid>> {
    for pair in cookie_header.split(';') {
        let pair = pair.trim();
        if let Some(value) = pair.strip_prefix("ione_session=") {
            return Some(Uuid::parse_str(value).ok());
        }
    }
    None
}
