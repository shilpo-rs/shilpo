//! Ownership types and erasure helpers for in-process credential buffers.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::Zeroize;

/// An owned UTF-8 credential buffer that is zeroized before its allocation is released.
///
/// The wrapper intentionally exposes borrowed text only. Keeping `Drop` on this leaf owner,
/// rather than on command enums, lets mailbox code move command fields without unsafe extraction.
pub struct SecretString {
    bytes: Vec<u8>,
    #[cfg(test)]
    wipe_observation: Option<WipeObservation>,
}

impl SecretString {
    /// Adopt an owned UTF-8 byte allocation without copying it.
    pub fn from_utf8(mut bytes: Vec<u8>) -> Result<Self, std::str::Utf8Error> {
        if let Err(error) = std::str::from_utf8(&bytes) {
            bytes.zeroize();
            return Err(error);
        }
        Ok(Self {
            bytes,
            #[cfg(test)]
            wipe_observation: None,
        })
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes).expect("SecretString preserves UTF-8")
    }

    #[cfg(test)]
    pub(crate) fn new_observed_for_test(value: &str) -> (Self, WipeObservation) {
        let observation = WipeObservation::default();
        (
            Self {
                bytes: value.as_bytes().to_vec(),
                wipe_observation: Some(observation.clone()),
            },
            observation,
        )
    }
}

impl Clone for SecretString {
    fn clone(&self) -> Self {
        Self {
            bytes: self.bytes.clone(),
            #[cfg(test)]
            wipe_observation: self.wipe_observation.clone(),
        }
    }
}

impl PartialEq for SecretString {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl Eq for SecretString {}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString(<redacted>)")
    }
}

impl From<String> for SecretString {
    fn from(value: String) -> Self {
        Self {
            bytes: value.into_bytes(),
            #[cfg(test)]
            wipe_observation: None,
        }
    }
}

impl From<&str> for SecretString {
    fn from(value: &str) -> Self {
        Self {
            bytes: value.as_bytes().to_vec(),
            #[cfg(test)]
            wipe_observation: None,
        }
    }
}

impl Serialize for SecretString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SecretString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Self::from)
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        self.bytes.as_mut_slice().zeroize();
        #[cfg(test)]
        if let Some(observation) = &self.wipe_observation {
            observation.record(&self.bytes);
        }
        // `Vec::zeroize` also clears and wipes spare capacity, covering the whole owned
        // allocation rather than only the initialized credential bytes observed above.
        self.bytes.zeroize();
    }
}

/// Zeroizes a mutable string allocation and clears its logical contents.
pub fn zeroize_string(value: &mut String) {
    value.zeroize();
}

/// Zeroizes a mutable byte slice.
pub fn zeroize_bytes(value: &mut [u8]) {
    value.zeroize();
}

#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct WipeObservation(std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>>);

#[cfg(test)]
impl WipeObservation {
    fn record(&self, bytes: &[u8]) {
        *self.0.lock().unwrap() = Some(bytes.to_vec());
    }

    pub(crate) fn bytes(&self) -> Option<Vec<u8>> {
        self.0.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::SecretString;

    #[test]
    fn from_utf8_adopts_the_allocation_without_copying() {
        let bytes = b"moved-sensitive-response".to_vec();
        let allocation = bytes.as_ptr();

        let secret = SecretString::from_utf8(bytes).unwrap();

        assert_eq!(secret.as_str(), "moved-sensitive-response");
        assert_eq!(secret.as_str().as_ptr(), allocation);
    }
}
