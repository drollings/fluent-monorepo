use crate::transforms::{TransformError, TransformStrategy};
use fluent_types::openai_wire::RouterRequest;

pub struct NoTransform;

impl TransformStrategy for NoTransform {
    fn name(&self) -> &str {
        "none"
    }

    fn transform(&self, request: &RouterRequest) -> Result<RouterRequest, TransformError> {
        Ok(request.clone())
    }
}
