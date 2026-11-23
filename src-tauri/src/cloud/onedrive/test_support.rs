//! A scripted HTTP transport: replies are matched by method and URL prefix,
//! the longest prefix wins, and a route's last reply repeats.

use crate::cloud::{CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, QuarkError};
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::Mutex;

struct Route {
    method: &'static str,
    prefix: String,
    replies: VecDeque<(u16, String)>,
}

#[derive(Default)]
pub(super) struct ScriptedHttp {
    routes: Mutex<Vec<Route>>,
    pub requests: Mutex<Vec<CloudHttpRequest>>,
}

impl ScriptedHttp {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on(self, method: &'static str, prefix: &str, replies: Vec<(u16, Value)>) -> Self {
        self.routes.lock().unwrap().push(Route {
            method,
            prefix: prefix.to_owned(),
            replies: replies
                .into_iter()
                .map(|(status, body)| (status, body.to_string()))
                .collect(),
        });
        self
    }

    pub fn sent(&self, method: &str, prefix: &str) -> Vec<CloudHttpRequest> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.method == method && request.url.starts_with(prefix))
            .cloned()
            .collect()
    }
}

impl CloudHttpTransport for ScriptedHttp {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        self.requests.lock().unwrap().push(request.clone());
        let mut routes = self.routes.lock().unwrap();
        let route = routes
            .iter_mut()
            .filter(|route| {
                route.method == request.method && request.url.starts_with(&route.prefix)
            })
            .max_by_key(|route| route.prefix.len())
            .unwrap_or_else(|| panic!("unscripted request {} {}", request.method, request.url));
        let (status, body) = if route.replies.len() > 1 {
            route.replies.pop_front().unwrap()
        } else {
            route.replies.front().cloned().unwrap()
        };
        Ok(CloudHttpResponse {
            status,
            body,
            set_cookies: vec![],
        })
    }
}
