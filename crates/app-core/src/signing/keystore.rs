//! Signing identities kept in a key store (the system's, or a stand-in for tests), as the
//! sign dialog offers them: only certificates fit for signing, each with where it is kept.

use std::sync::Arc;

use pagebara_i18n::tr;
use pagebara_sign::{
    Certificate, CertificateRequest, Identity, KeyUsage, PrivateKey, SignatureScheme, Signer, Time,
};

/// Where signing identities with their private keys come from.
pub trait KeyStore: Send + Sync {
    /// Every identity the store holds, fit for signing or not.
    fn identities(&self) -> Result<Vec<Identity>, String>;
}

/// The operating system's store: the Windows certificate store, the macOS Keychain; none
/// elsewhere.
#[derive(Debug)]
pub struct SystemKeyStore;

impl KeyStore for SystemKeyStore {
    fn identities(&self) -> Result<Vec<Identity>, String> {
        pagebara_sign::system_identities().map_err(|e| e.to_string())
    }
}

/// An identity from a key store, described for the sign dialog.
#[derive(Clone, Debug)]
pub struct StoreIdentity {
    pub identity: Arc<Identity>,
    /// Where the key is kept, e.g. "Windows certificate store".
    pub location: String,
    /// The signer's name (and e-mail).
    pub subject: String,
    pub issuer: String,
    /// The last day of validity, `2027-11-12`.
    pub expires: String,
}

impl StoreIdentity {
    pub fn new(identity: Identity) -> StoreIdentity {
        let c = &identity.certificate;
        let location = match &identity.key {
            PrivateKey::External(s, _) => s.location(),
            _ => tr!("key store"),
        };
        let subject = match c.email() {
            Some(e) => format!("{} <{e}>", c.display_name()),
            None => c.display_name(),
        };
        StoreIdentity {
            location,
            subject,
            issuer: c.issuer_display_name(),
            expires: c.not_after().to_iso()[..10].to_owned(),
            identity: Arc::new(identity),
        }
    }

    /// For the identity choice: "Ann Example – Windows certificate store".
    pub fn label(&self) -> String {
        format!("{} – {}", self.subject, self.location)
    }
}

/// Whether `c` may sign documents at `now`: within its validity, not a CA's, and with the
/// digital signature or non-repudiation key usage (or no key usage restriction).
pub fn fit_for_signing(c: &Certificate, now: Time) -> bool {
    c.is_valid_at(now)
        && !c.is_ca()
        && c.key_usage()
            .is_none_or(|k| k.has(KeyUsage::DIGITAL_SIGNATURE) || k.has(KeyUsage::NON_REPUDIATION))
}

/// The identities of `store` fit for signing at `now`, by name.
pub fn signing_identities(store: &dyn KeyStore, now: Time) -> Result<Vec<StoreIdentity>, String> {
    let mut out: Vec<StoreIdentity> = store
        .identities()?
        .into_iter()
        .filter(|i| fit_for_signing(&i.certificate, now))
        .map(StoreIdentity::new)
        .collect();
    out.sort_by(|a, b| a.subject.cmp(&b.subject).then(a.expires.cmp(&b.expires)));
    Ok(out)
}

/// A stand-in for the system's store (tests, `PAGEBARA_KEYSTORE=test`): identities made at
/// start, whose keys sign through a [`Signer`] as a system store's do. Besides the usable
/// "Test Store Signer" (issued by "Test Store CA") it holds an expired certificate and one
/// for encryption only, which the dialog must leave out.
#[derive(Debug)]
pub struct TestKeyStore {
    identities: Vec<Identity>,
}

impl TestKeyStore {
    pub fn new() -> Result<TestKeyStore, String> {
        let e = |e: pagebara_sign::Error| e.to_string();
        let key = || PrivateKey::generate_p256().map_err(e);
        let ca = Identity::self_signed(&CertificateRequest::ca("Test Store CA", 3650), key()?)
            .map_err(e)?;
        let mut signer = CertificateRequest::signer("Test Store Signer", 365);
        signer.email = Some("store@example.org".into());
        let mut expired = CertificateRequest::signer("Expired Signer", 365);
        expired.not_before = Time(Time::now().0 - 400 * 86400);
        expired.not_after = Time(Time::now().0 - 30 * 86400);
        let mut encryption = CertificateRequest::signer("Encryption Only", 365);
        encryption.key_usage = KeyUsage(KeyUsage::KEY_ENCIPHERMENT);
        let identities = [signer, expired, encryption]
            .iter()
            .map(|req| {
                let id = ca.issue(req, key()?).map_err(e)?;
                let signer = Arc::new(TestSigner(id.key));
                Ok(Identity {
                    key: PrivateKey::external(signer).map_err(e)?,
                    ..id
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(TestKeyStore { identities })
    }
}

impl KeyStore for TestKeyStore {
    fn identities(&self) -> Result<Vec<Identity>, String> {
        Ok(self.identities.clone())
    }
}

/// A key that only signs on request, like one a system store keeps.
struct TestSigner(PrivateKey);

impl Signer for TestSigner {
    fn public_key_der(&self) -> pagebara_sign::Result<Vec<u8>> {
        self.0.public_key_der()
    }

    fn sign_prehash(&self, scheme: SignatureScheme, hash: &[u8]) -> pagebara_sign::Result<Vec<u8>> {
        self.0.sign_prehash(scheme, hash)
    }

    fn location(&self) -> String {
        "Test key store".into()
    }
}
