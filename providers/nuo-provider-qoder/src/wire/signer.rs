//! Qoder's COSY request-signing protocol (`api*.qoder.sh`).

use base64::Engine as _;
use cbc::cipher::{BlockEncryptMut, KeyIvInit};
use md5::Md5;
use nuo_model_codec::{PreflightValidator, ProviderError, ProviderErrorKind, ResolvedAuth};
use nuo_provider_transport::pipeline::RequestSignerPhase;
use nuo_provider_transport::request::RequestBuilder;
use sha2::Digest;
use std::any::TypeId;

use super::super::identity::QoderRequestIdentity;
use super::super::surface::{
    COSY_VERSION, IDENTITY_HEADERS, INFERENCE_PATH, INFERENCE_QUERY, SIGNED_PATH, VERSION_HEADER,
};

const QODER_RSA_PUBLIC_KEY_B64: &str = "MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDA8iMH5c02LilrsERw9t6Pv5Nc4k6Pz1EaDicBMpdpxKduSZu5OANqUq8er4GM95omAGIOPOh+Nx0spthYA2BqGz+l6HRkPJ7S236FZz73In/KVuLnwI8JJ2CbuJap8kvheCCZpmAWpb/cPx/3Vr/J6I17XcW+ML9FoCI6AOvOzwIDAQAB";
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

#[allow(dead_code)]
pub fn inference_url(base: &str) -> String {
    let query = INFERENCE_QUERY
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&");
    format!("{}{INFERENCE_PATH}?{query}", base.trim_end_matches('/'))
}

#[allow(dead_code)]
pub fn signed_path_for(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let path_query = rest.split_once('/').map(|(_, p)| p).unwrap_or("");
    let path = path_query.split(['?', '#']).next().unwrap_or("");
    let path = format!("/{path}");
    path.strip_prefix("/algo")
        .map(str::to_string)
        .unwrap_or(path)
}

/// Per-machine COSY identity: the AES key/IV and its RSA-wrapped form.
#[derive(Clone)]
pub struct CosyIdentity {
    machine_key_hex: String,
    key_b64: String,
}

impl CosyIdentity {
    #[allow(dead_code)]
    pub fn generate() -> Self {
        let machine_key_hex = generate_machine_key_hex();
        Self::from_machine_key_hex(machine_key_hex).expect("generated hex is 32 chars")
    }

    pub fn from_machine_key_hex(machine_key_hex: String) -> Option<Self> {
        if machine_key_hex.len() != 32
            || !machine_key_hex
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return None;
        }
        let key = key_bytes(&machine_key_hex);
        let key_b64 = rsa_encrypt_with_pinned_key(&key);
        Some(Self {
            machine_key_hex,
            key_b64,
        })
    }

    pub fn parse(machine_key_hex: &str) -> Option<Self> {
        Self::from_machine_key_hex(machine_key_hex.to_string())
    }

    #[allow(dead_code)]
    pub fn key_hex(&self) -> &str {
        &self.machine_key_hex
    }

    #[allow(dead_code)]
    pub fn key_hex_arr(&self) -> [u8; 16] {
        key_bytes(&self.machine_key_hex)
    }

    fn encrypt_info(&self, plaintext: &str) -> String {
        type Aes128Cbc = cbc::Encryptor<aes::Aes128>;
        let key = key_bytes(&self.machine_key_hex);
        let mut out = vec![0u8; plaintext.len() + 16];
        let ct = Aes128Cbc::new_from_slices(&key, &key)
            .expect("16-byte key and IV")
            .encrypt_padded_b2b_mut::<cbc::cipher::block_padding::Pkcs7>(
                plaintext.as_bytes(),
                &mut out,
            )
            .expect("exact block multiple");
        base64::engine::general_purpose::STANDARD.encode(ct)
    }
}

fn key_bytes(machine_key_hex: &str) -> [u8; 16] {
    let mut key = [0u8; 16];
    key.copy_from_slice(&machine_key_hex.as_bytes()[..16]);
    key
}

fn fill_hex(out: &mut [u8]) {
    let mut state = fastrand_entropy();
    for slot in out {
        *slot = HEX_DIGITS[(state & 0xf) as usize];
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        if state == 0 {
            state = fastrand_entropy();
        }
    }
}

pub fn generate_machine_key_hex() -> String {
    let mut raw = [0u8; 32];
    fill_hex(&mut raw);
    String::from_utf8(raw.to_vec()).expect("hex digits are valid ASCII")
}

fn rsa_encrypt_with_pinned_key(data: &[u8]) -> String {
    use rsa::pkcs8::DecodePublicKey;
    let der = base64::engine::general_purpose::STANDARD
        .decode(QODER_RSA_PUBLIC_KEY_B64)
        .expect("pinned key is valid base64");
    let key = rsa::RsaPublicKey::from_public_key_der(&der).expect("pinned SPKI key parses");
    let mut rng = QoderRng;
    let ciphertext = key
        .encrypt(&mut rng, rsa::Pkcs1v15Encrypt, data)
        .expect("encryption succeeds");
    base64::engine::general_purpose::STANDARD.encode(ciphertext)
}

struct QoderRng;

impl rsa::rand_core::RngCore for QoderRng {
    fn next_u32(&mut self) -> u32 {
        fastrand::u32(..)
    }

    fn next_u64(&mut self) -> u64 {
        fastrand::u64(..)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for chunk in dest.chunks_mut(8) {
            let val = fastrand_entropy().to_ne_bytes();
            let len = chunk.len().min(val.len());
            chunk[..len].copy_from_slice(&val[..len]);
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rsa::rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl rsa::rand_core::CryptoRng for QoderRng {}

fn fastrand_entropy() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let random = fastrand::u64(..);
    (now as u64) ^ random
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct PreparedCosy {
    pub authorization: String,
    pub date: String,
    pub key: String,
    pub payload_b64: String,
    pub request_id: String,
    pub signature_hex: String,
}

#[allow(dead_code)]
pub fn prepare_request(
    identity: &CosyIdentity,
    identity_json: &str,
    uid: &str,
    encoded_body: &str,
    now_secs: u64,
) -> PreparedCosy {
    prepare_request_for_path(
        identity,
        identity_json,
        uid,
        encoded_body,
        SIGNED_PATH,
        now_secs,
    )
}

pub fn prepare_request_for_path(
    identity: &CosyIdentity,
    identity_json: &str,
    _uid: &str,
    encoded_body: &str,
    signed_path: &str,
    now_secs: u64,
) -> PreparedCosy {
    let info = identity.encrypt_info(identity_json);
    let request_id = mint_cosy_request_id();
    let auth_payload = serde_json::json!({
        "version": "v1",
        "requestId": request_id,
        "info": info,
        "cosyVersion": COSY_VERSION,
        "ideVersion": "",
    })
    .to_string();

    let payload_b64 = base64::engine::general_purpose::STANDARD.encode(auth_payload.as_bytes());
    let sig_input = format!(
        "{payload_b64}\n{}\n{now_secs}\n{encoded_body}\n{signed_path}",
        identity.key_b64
    );

    let mut hasher = Md5::new();
    hasher.update(sig_input.as_bytes());
    let digest = hasher.finalize();
    let signature_hex = format!("{digest:x}");

    PreparedCosy {
        authorization: format!("Bearer COSY.{payload_b64}.{signature_hex}"),
        date: now_secs.to_string(),
        key: identity.key_b64.clone(),
        payload_b64,
        request_id,
        signature_hex,
    }
}

fn mint_cosy_request_id() -> String {
    let mut raw = [0u8; 16];
    for chunk in raw.chunks_mut(8) {
        let val = fastrand_entropy().to_ne_bytes();
        let len = chunk.len().min(val.len());
        chunk[..len].copy_from_slice(&val[..len]);
    }
    raw[6] = (raw[6] & 0x0f) | 0x40;
    raw[8] = (raw[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        raw[0], raw[1], raw[2], raw[3],
        raw[4], raw[5],
        raw[6], raw[7],
        raw[8], raw[9],
        raw[10], raw[11], raw[12], raw[13], raw[14], raw[15]
    )
}

/// Universal COSY transport signer for Qoder (ADR-0267).
#[derive(Debug, Clone, Default)]
pub struct CosyTransportSigner;

impl PreflightValidator for CosyTransportSigner {
    fn required_extensions(&self) -> Vec<TypeId> {
        vec![TypeId::of::<QoderRequestIdentity>()]
    }
}

impl RequestSignerPhase for CosyTransportSigner {
    fn clone_box_signer(&self) -> Box<dyn RequestSignerPhase> {
        Box::new(self.clone())
    }

    /// The inference URL: the surface's declared path and fixed query over
    /// the endpoint the resolved identity elects — the center region map's
    /// `inferNodes` choice (§3.1a) when synced, else the executor's
    /// (pinned) base URL. The signature binds the request to whichever host
    /// serves it, so the override must happen here where both are derived.
    fn request_url(&self, base_url: &str, auth: &ResolvedAuth) -> String {
        match auth
            .extension::<QoderRequestIdentity>()
            .and_then(|identity| identity.infer_endpoint.clone())
        {
            Some(elected) => inference_url(&elected),
            None => inference_url(base_url),
        }
    }

    fn sign_request(
        &self,
        mut req: RequestBuilder,
        body_bytes: &[u8],
        auth: &ResolvedAuth,
    ) -> Result<RequestBuilder, ProviderError> {
        let qoder_id = auth.extension::<QoderRequestIdentity>().ok_or_else(|| {
            ProviderError::new(
                "qoder",
                ProviderErrorKind::Authentication,
                "missing QoderRequestIdentity in ResolvedAuth extensions",
            )
        })?;

        let cosy = CosyIdentity::parse(qoder_id.machine_key_hex.expose_secret()).ok_or_else(|| {
            ProviderError::new(
                "qoder",
                ProviderErrorKind::Authentication,
                "malformed Qoder machine key",
            )
        })?;

        let email = auth.user_email.as_deref().unwrap_or("");
        let bearer = auth.token.expose_secret();
        let identity_json = qoder_id.identity_payload_json(bearer, email);

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let encoded_body = std::str::from_utf8(body_bytes).unwrap_or("");
        let prepared = prepare_request_for_path(
            &cosy,
            &identity_json,
            &qoder_id.uid,
            encoded_body,
            SIGNED_PATH,
            now_secs,
        );

        // Stamp static identity headers
        for (name, value) in IDENTITY_HEADERS {
            req = req.header(*name, *value);
        }
        req = req.header(VERSION_HEADER, COSY_VERSION);

        // Stamp COSY signature headers
        req = req
            .header("Authorization", prepared.authorization)
            .header("Cosy-Date", prepared.date)
            .header("Cosy-Key", prepared.key);

        if !qoder_id.uid.is_empty() {
            req = req.header("Cosy-User", qoder_id.uid.clone());
        }
        if let Some(org_id) = &qoder_id.organization_id {
            req = req.header("Cosy-Organization-Id", org_id.clone());
        }
        if !qoder_id.organization_tags.is_empty() {
            req = req.header("Cosy-Organization-Tags", qoder_id.organization_tags.join(","));
        }

        Ok(req)
    }
}
