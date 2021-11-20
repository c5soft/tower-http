use crate::{
    classify::{MakeClassifier, ServerErrorsFailureClass},
    trace::{MakeSpan, OnBodyChunk, OnEos, OnFailure, OnRequest, OnResponse, TraceLayer},
};
use http::{
    header, uri::Scheme, Extensions, HeaderMap, Method, Request, Response, StatusCode, Uri, Version,
};
use std::{borrow::Cow, sync::Arc, time::Duration};
use tracing::{field::Empty, Span};

pub struct Config {
    scheme: Cow<'static, str>,
    // these are trait objects such that we can add more callbacks without
    // adding more type params, which would be a breaking change
    extract_matched_path: Arc<dyn ExtractMatchedPath>,
    extract_client_ip: Arc<dyn ExtractClientIp>,
    set_otel_parent: Arc<dyn SetOtelParent>,
}

impl Config {
    pub fn scheme(mut self, scheme: Scheme) -> Self {
        self.scheme = http_scheme(&scheme);
        self
    }

    pub fn extract_matched_path<T>(mut self, extract_matched_path: T) -> Config
    where
        T: ExtractMatchedPath,
    {
        self.extract_matched_path = Arc::new(extract_matched_path);
        self
    }

    pub fn extract_client_ip<T>(mut self, extract_client_ip: T) -> Config
    where
        T: ExtractClientIp,
    {
        self.extract_client_ip = Arc::new(extract_client_ip);
        self
    }

    pub fn set_otel_parent<T>(mut self, set_otel_parent: T) -> Config
    where
        T: SetOtelParent,
    {
        self.set_otel_parent = Arc::new(set_otel_parent);
        self
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scheme: http_scheme(&Scheme::HTTP),
            extract_matched_path: Arc::new(DefaultExtractMatchedPath),
            extract_client_ip: Arc::new(DefaultExtractClientIp),
            set_otel_parent: Arc::new(DefaultSetOtelParent),
        }
    }
}

impl<M> TraceLayer<M> {
    pub fn opentelemetry_server(
        self,
    ) -> TraceLayer<
        M,
        OtelMakeSpan,
        OtelOnRequest,
        OtelOnResponse,
        OtelOnBodyChunk,
        OtelOnEos,
        OtelOnFailure,
    >
    where
        M: MakeClassifier,
        M::FailureClass: FailureDetails,
    {
        let config = Config::default();

        TraceLayer {
            make_classifier: self.make_classifier,
            make_span: OtelMakeSpan {
                scheme: config.scheme,
                extract_matched_path: config.extract_matched_path,
                extract_client_ip: config.extract_client_ip,
                set_otel_parent: config.set_otel_parent,
            },
            on_request: OtelOnRequest,
            on_response: OtelOnResponse,
            on_body_chunk: OtelOnBodyChunk,
            on_eos: OtelOnEos,
            on_failure: OtelOnFailure,
        }
    }
}

impl<M>
    TraceLayer<
        M,
        OtelMakeSpan,
        OtelOnRequest,
        OtelOnResponse,
        OtelOnBodyChunk,
        OtelOnEos,
        OtelOnFailure,
    >
{
    pub fn configure(
        mut self,
        config: Config,
    ) -> TraceLayer<
        M,
        OtelMakeSpan,
        OtelOnRequest,
        OtelOnResponse,
        OtelOnBodyChunk,
        OtelOnEos,
        OtelOnFailure,
    >
    where
        M: MakeClassifier,
        M::FailureClass: FailureDetails,
    {
        let Config {
            scheme,
            extract_matched_path,
            extract_client_ip,
            set_otel_parent,
        } = config;

        self.make_span.scheme = scheme;
        self.make_span.extract_matched_path = extract_matched_path;
        self.make_span.extract_client_ip = extract_client_ip;
        self.make_span.set_otel_parent = set_otel_parent;

        self
    }
}

#[derive(Clone)]
pub struct OtelMakeSpan {
    scheme: Cow<'static, str>,
    extract_matched_path: Arc<dyn ExtractMatchedPath>,
    extract_client_ip: Arc<dyn ExtractClientIp>,
    set_otel_parent: Arc<dyn SetOtelParent>,
}

impl<B> MakeSpan<B> for OtelMakeSpan {
    fn make_span(&mut self, request: &Request<B>) -> Span {
        let user_agent = request
            .headers()
            .get(header::USER_AGENT)
            .map(|h| h.to_str().unwrap_or(""))
            .unwrap_or("");

        let host = request
            .headers()
            .get(header::HOST)
            .map(|h| h.to_str().unwrap_or(""))
            .unwrap_or("");

        let http_route = self
            .extract_matched_path
            .extract_matched_path(request.uri(), request.extensions());

        let client_ip = self
            .extract_client_ip
            .extract_client_ip(request.headers(), request.extensions())
            .unwrap_or_default();

        let span = tracing::info_span!(
            "HTTP request",
            http.method = %http_method(request.method()),
            http.route = %http_route,
            http.flavor = %http_flavor(request.version()),
            http.scheme = %self.scheme,
            http.host = %host,
            http.client_ip = %client_ip,
            http.user_agent = %user_agent,
            http.target = %request.uri().path_and_query().map(|p| p.as_str()).unwrap_or(""),
            http.status_code = Empty,
            otel.kind = "server",
            otel.status_code = Empty,
            trace_id = Empty,
            // requires https://github.com/tower-rs/tower-http/pull/150
            // request_id = %request_id,
            exception.message = Empty,
            exception.details = Empty,
        );

        self.set_otel_parent
            .set_otel_parent(request.headers(), &span);

        span
    }
}

fn http_method(method: &Method) -> Cow<'static, str> {
    match method {
        &Method::CONNECT => "CONNECT".into(),
        &Method::DELETE => "DELETE".into(),
        &Method::GET => "GET".into(),
        &Method::HEAD => "HEAD".into(),
        &Method::OPTIONS => "OPTIONS".into(),
        &Method::PATCH => "PATCH".into(),
        &Method::POST => "POST".into(),
        &Method::PUT => "PUT".into(),
        &Method::TRACE => "TRACE".into(),
        other => other.to_string().into(),
    }
}

pub trait ExtractMatchedPath: Send + Sync + 'static {
    fn extract_matched_path<'a>(&self, uri: &'a Uri, extensions: &'a Extensions) -> Cow<'a, str>;
}

#[derive(Clone, Copy)]
struct DefaultExtractMatchedPath;

impl ExtractMatchedPath for DefaultExtractMatchedPath {
    #[inline]
    fn extract_matched_path<'a>(&self, uri: &'a Uri, _extensions: &'a Extensions) -> Cow<'a, str> {
        uri.path().into()
    }
}

pub trait ExtractClientIp: Send + Sync + 'static {
    fn extract_client_ip<'a>(
        &self,
        headers: &'a HeaderMap,
        extensions: &'a Extensions,
    ) -> Option<Cow<'a, str>>;
}

#[derive(Clone, Copy)]
struct DefaultExtractClientIp;

impl ExtractClientIp for DefaultExtractClientIp {
    #[inline]
    fn extract_client_ip<'a>(
        &self,
        _headers: &'a HeaderMap,
        _extensions: &'a Extensions,
    ) -> Option<Cow<'a, str>> {
        None
    }
}

// NOTE: should also record trace_id on span a la
// https://github.com/LukeMathWalker/tracing-actix-web/blob/352c274c8da1a9dec8757fc254deae5c689d408f/src/otel.rs#L43-L55
pub trait SetOtelParent: Send + Sync + 'static {
    fn set_otel_parent(&self, headers: &HeaderMap, span: &Span);
}

#[derive(Clone, Copy)]
struct DefaultSetOtelParent;

impl SetOtelParent for DefaultSetOtelParent {
    #[inline]
    fn set_otel_parent(&self, _headers: &HeaderMap, _span: &Span) {}
}

fn http_flavor(version: Version) -> Cow<'static, str> {
    match version {
        Version::HTTP_09 => "0.9".into(),
        Version::HTTP_10 => "1.0".into(),
        Version::HTTP_11 => "1.1".into(),
        Version::HTTP_2 => "2.0".into(),
        Version::HTTP_3 => "3.0".into(),
        other => format!("{:?}", other).into(),
    }
}

fn http_scheme(scheme: &Scheme) -> Cow<'static, str> {
    if scheme == &Scheme::HTTP {
        "http".into()
    } else if scheme == &Scheme::HTTPS {
        "https".into()
    } else {
        scheme.to_string().into()
    }
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct OtelOnRequest;

impl<B> OnRequest<B> for OtelOnRequest {
    fn on_request(&mut self, _request: &Request<B>, _span: &Span) {}
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct OtelOnResponse;

impl<B> OnResponse<B> for OtelOnResponse {
    fn on_response(self, response: &Response<B>, _latency: Duration, span: &Span) {
        let status = response.status().as_u16().to_string();
        span.record("http.status_code", &tracing::field::display(status));
    }
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct OtelOnBodyChunk;

impl<B> OnBodyChunk<B> for OtelOnBodyChunk {
    #[inline]
    fn on_body_chunk(&mut self, _chunk: &B, _latency: Duration, _span: &Span) {}
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct OtelOnEos;

impl OnEos for OtelOnEos {
    #[inline]
    fn on_eos(self, _trailers: Option<&HeaderMap>, _stream_duration: Duration, _span: &Span) {}
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct OtelOnFailure;

impl<E> OnFailure<E> for OtelOnFailure
where
    E: FailureDetails,
{
    fn on_failure(&mut self, failure: E, _latency: Duration, span: &Span) {
        if let Some(status) = failure.status() {
            if status.is_server_error() {
                span.record("otel.status_code", &"ERROR");
            }
        } else {
            span.record("otel.status_code", &"ERROR");
        }

        if let Some(message) = failure.message() {
            span.record("exception.message", &tracing::field::display(message));
        }

        if let Some(details) = failure.details() {
            span.record("exception.details", &tracing::field::display(details));
        }
    }
}

pub trait FailureDetails {
    fn status(&self) -> Option<StatusCode>;

    fn message(&self) -> Option<String>;

    fn details(&self) -> Option<String>;
}

impl FailureDetails for ServerErrorsFailureClass {
    fn status(&self) -> Option<StatusCode> {
        match self {
            ServerErrorsFailureClass::StatusCode(status) => Some(*status),
            ServerErrorsFailureClass::Error(_) => None,
        }
    }

    fn message(&self) -> Option<String> {
        match self {
            ServerErrorsFailureClass::StatusCode(_) => None,
            ServerErrorsFailureClass::Error(err) => Some(err.to_owned()),
        }
    }

    #[inline]
    fn details(&self) -> Option<String> {
        None
    }
}