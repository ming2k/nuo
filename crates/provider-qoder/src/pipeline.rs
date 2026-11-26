//! Builder for Qoder's complete phased transport pipeline.

use provider::ProviderPipeline;

use super::wire::{
    CosyTransportSigner, QoderAgentEnvelope, QoderBodyCodec, QoderStreamTransformer,
};

/// Construct a fully wired ProviderPipeline for Qoder.
pub fn build_qoder_pipeline() -> ProviderPipeline {
    ProviderPipeline::builder()
        .with_envelope(QoderAgentEnvelope)
        .with_codec(QoderBodyCodec)
        .with_signer(CosyTransportSigner)
        .with_validator(CosyTransportSigner)
        .with_stream_transformer(QoderStreamTransformer)
        .build()
}
