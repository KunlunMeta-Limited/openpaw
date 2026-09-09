//! FallbackCompressProvider — wraps a primary LlmProvider and falls back
//! to a cheaper model when the primary fails with a context-length error
//! (same-source port; the fallback is model-level, matching the upstream
//! `[model] primary + fallbacks` config shape).

use async_trait::async_trait;
use futures::stream::BoxStream;
use openpaw_core::{
    Capabilities, CompletionRequest, CompletionResponse, Error, LlmProvider, Result, StreamEvent,
};

pub struct FallbackCompressProvider {
    primary: Arc<dyn LlmProvider>,
    fallback_model: String,
    max_retries: u32,
}

use std::sync::Arc;

impl FallbackCompressProvider {
    pub fn new(primary: Arc<dyn LlmProvider>, fallback_model: impl Into<String>) -> Self {
        Self {
            primary,
            fallback_model: fallback_model.into(),
            max_retries: 1,
        }
    }

    pub fn with_max_retries(mut self, n: u32) -> Self {
        self.max_retries = n;
        self
    }

    fn is_context_length_error(err: &Error) -> bool {
        // Structured variant first (providers return Error::ContextTooLong).
        if err.is_context_too_long() {
            return true;
        }
        // String matching for providers that still surface plain Provider
        // errors.
        let msg = format!("{err}").to_lowercase();
        msg.contains("context_length")
            || msg.contains("context length")
            || msg.contains("too many tokens")
            || msg.contains("max_context")
            || msg.contains("context_too_long")
            || msg.contains("token limit")
            || msg.contains("maximum context")
    }
}

#[async_trait]
impl LlmProvider for FallbackCompressProvider {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        let mut attempts = 0;
        let mut original_failed = false;
        let mut last_error: Option<Error> = None;
        loop {
            // Only try the original model if it hasn't failed yet; once it
            // fails with a context-length error, retrying it is pointless.
            if !original_failed {
                match self.primary.complete(req.clone()).await {
                    Ok(resp) => return Ok(resp),
                    Err(e) if Self::is_context_length_error(&e) => {
                        original_failed = true;
                        last_error = Some(e);
                    }
                    Err(e) => return Err(e),
                }
            }

            if attempts < self.max_retries {
                attempts += 1;
                let mut fallback_req = req.clone();
                fallback_req.model = self.fallback_model.clone();
                match self.primary.complete(fallback_req).await {
                    Ok(resp) => return Ok(resp),
                    Err(e2)
                        if Self::is_context_length_error(&e2) && attempts < self.max_retries =>
                    {
                        last_error = Some(e2);
                        continue;
                    }
                    Err(e2) => return Err(e2),
                }
            }

            return Err(last_error.unwrap_or_else(|| {
                Error::Provider("context length error: retries exhausted".into())
            }));
        }
    }

    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        let mut attempts = 0;
        loop {
            match self.primary.stream(req.clone()).await {
                Ok(stream) => return Ok(stream),
                Err(e) if Self::is_context_length_error(&e) => {
                    if attempts < self.max_retries {
                        attempts += 1;
                        let mut fallback_req = req.clone();
                        fallback_req.model = self.fallback_model.clone();
                        match self.primary.stream(fallback_req).await {
                            Ok(stream) => return Ok(stream),
                            Err(e2)
                                if Self::is_context_length_error(&e2)
                                    && attempts < self.max_retries =>
                            {
                                continue;
                            }
                            Err(e2) => return Err(e2),
                        }
                    }
                    return Err(e);
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn capabilities(&self) -> Capabilities {
        self.primary.capabilities()
    }

    fn name(&self) -> &str {
        self.primary.name()
    }

    fn default_model(&self) -> &str {
        self.primary.default_model()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openpaw_core::{ContentBlock, StopReason, Usage};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Provider that fails with a context-length error for `fail_model`
    /// and succeeds for `ok_model`.
    struct ModelSwitchMock {
        fail_model: String,
        ok_model: String,
        complete_calls: AtomicUsize,
    }

    #[async_trait]
    impl LlmProvider for ModelSwitchMock {
        async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
            self.complete_calls.fetch_add(1, Ordering::SeqCst);
            if req.model == self.fail_model {
                return Err(Error::Provider(
                    "context_length_exceeded: too many tokens".into(),
                ));
            }
            if req.model == self.ok_model {
                return Ok(CompletionResponse {
                    content: vec![ContentBlock::Text { text: "ok".into() }],
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                });
            }
            Err(Error::Provider("unexpected model".into()))
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities::default()
        }

        fn name(&self) -> &str {
            "mock"
        }

        fn default_model(&self) -> &str {
            &self.fail_model
        }
    }

    fn req(model: &str) -> CompletionRequest {
        CompletionRequest {
            model: model.to_string(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn falls_back_on_context_error() {
        let primary = Arc::new(ModelSwitchMock {
            fail_model: "big-model".into(),
            ok_model: "small-model".into(),
            complete_calls: AtomicUsize::new(0),
        });
        let fb = FallbackCompressProvider::new(
            Arc::clone(&primary) as Arc<dyn LlmProvider>,
            "small-model",
        );
        let resp = fb.complete(req("big-model")).await.unwrap();
        assert_eq!(resp.text(), "ok");
        assert_eq!(primary.complete_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn passes_through_success() {
        // The primary model never hits the context error → single call.
        let primary = Arc::new(ModelSwitchMock {
            fail_model: "unused".into(),
            ok_model: "good-model".into(),
            complete_calls: AtomicUsize::new(0),
        });
        let fb =
            FallbackCompressProvider::new(Arc::clone(&primary) as Arc<dyn LlmProvider>, "fallback");
        let resp = fb.complete(req("good-model")).await.unwrap();
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
        assert_eq!(primary.complete_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn non_context_errors_propagate_without_fallback() {
        struct AuthFail;
        #[async_trait]
        impl LlmProvider for AuthFail {
            async fn complete(&self, _req: CompletionRequest) -> Result<CompletionResponse> {
                Err(Error::auth_failed(401, "bad key"))
            }
            fn capabilities(&self) -> Capabilities {
                Default::default()
            }
            fn name(&self) -> &str {
                "authfail"
            }
            fn default_model(&self) -> &str {
                "m"
            }
        }
        let fb = FallbackCompressProvider::new(Arc::new(AuthFail), "fallback");
        let err = fb.complete(req("m")).await.unwrap_err();
        assert!(err.to_string().contains("authentication failed"));
    }

    #[test]
    fn detects_context_length_errors() {
        let cases = vec![
            ("context_length_exceeded", true),
            ("too many tokens in request", true),
            ("max_context_window exceeded", true),
            ("token limit reached", true),
            ("network timeout", false),
            ("api key invalid", false),
        ];
        for (msg, expected) in cases {
            let err = Error::Provider(msg.into());
            assert_eq!(
                FallbackCompressProvider::is_context_length_error(&err),
                expected,
                "msg: {msg}"
            );
        }
        // The structured variant is honored directly.
        assert!(FallbackCompressProvider::is_context_length_error(
            &Error::context_too_long("140k")
        ));
    }

    #[tokio::test]
    async fn stream_falls_back_on_context_error() {
        use futures::StreamExt;
        struct StreamMock {
            fail_model: String,
        }
        #[async_trait]
        impl LlmProvider for StreamMock {
            async fn complete(&self, _req: CompletionRequest) -> Result<CompletionResponse> {
                unreachable!("stream test")
            }
            async fn stream(
                &self,
                req: CompletionRequest,
            ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
                if req.model == self.fail_model {
                    return Err(Error::ContextTooLong("too long".into()));
                }
                let resp = CompletionResponse {
                    content: vec![ContentBlock::Text {
                        text: "fallback-stream".into(),
                    }],
                    stop_reason: StopReason::EndTurn,
                    usage: Default::default(),
                };
                Ok(Box::pin(futures::stream::iter(vec![Ok(
                    StreamEvent::Final(Box::new(resp)),
                )])))
            }
            fn capabilities(&self) -> Capabilities {
                Default::default()
            }
            fn name(&self) -> &str {
                "streammock"
            }
            fn default_model(&self) -> &str {
                "primary"
            }
        }
        let fb = FallbackCompressProvider::new(
            Arc::new(StreamMock {
                fail_model: "primary".into(),
            }),
            "small",
        );
        let stream = fb.stream(req("primary")).await.unwrap();
        let events: Vec<_> = stream.collect().await;
        match &events[0] {
            Ok(StreamEvent::Final(resp)) => assert_eq!(resp.text(), "fallback-stream"),
            other => panic!("expected Final, got {other:?}"),
        }
    }
}
