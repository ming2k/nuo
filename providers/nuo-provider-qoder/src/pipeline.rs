//! Builder for Qoder's complete phased transport pipeline.

use nuo_provider_transport::pipeline::TransportPipeline;

use super::wire::{CosyTransportSigner, QoderAgentEnvelope, QoderBodyCodec, QoderStreamTransformer};

/// Construct a fully wired PhasedTransportPipeline for Qoder.
pub fn build_qoder_pipeline() -> TransportPipeline {
    TransportPipeline::builder()
        .with_envelope(QoderAgentEnvelope)
        .with_codec(QoderBodyCodec)
        .with_signer(CosyTransportSigner)
        .with_validator(CosyTransportSigner)
        .with_stream_transformer(QoderStreamTransformer)
        .build()
}
