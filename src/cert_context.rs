//! Bindings to winapi's `PCCERT_CONTEXT` APIs.

use std::ffi::OsString;
use std::io;
use std::mem;
use std::os::windows::prelude::*;
use std::ptr;
use std::slice;
use crypt32;
use winapi;

use Inner;
use ncrypt_key::NcryptKey;
use crypt_prov::{CryptProv, ProviderType};

// FIXME https://github.com/retep998/winapi-rs/pull/318
const CRYPT_ACQUIRE_COMPARE_KEY_FLAG: winapi::DWORD = 0x4;
const CRYPT_ACQUIRE_SILENT_FLAG: winapi::DWORD = 0x40;
const CRYPT_ACQUIRE_ALLOW_NCRYPT_KEY_FLAG: winapi::DWORD = 0x10000;

// FIXME
const CRYPT_STRING_BASE64HEADER: winapi::DWORD = 0x0;

/// A supported hashing algorithm
pub struct HashAlgorithm(winapi::DWORD, usize);

#[allow(missing_docs)]
impl HashAlgorithm {
    pub fn md5() -> HashAlgorithm {
        HashAlgorithm(winapi::CALG_MD5, 16)
    }

    pub fn sha1() -> HashAlgorithm{
        HashAlgorithm(winapi::CALG_SHA1, 20)
    }

    pub fn sha256() -> HashAlgorithm {
        HashAlgorithm(winapi::CALG_SHA_256, 32)
    }

    pub fn sha384() -> HashAlgorithm {
        HashAlgorithm(winapi::CALG_SHA_384, 48)
    }

    pub fn sha512() -> HashAlgorithm {
        HashAlgorithm(winapi::CALG_SHA_512, 64)
    }
}

/// Wrapper of a winapi certificate, or a `PCCERT_CONTEXT`.
#[derive(Debug)]
pub struct CertContext(winapi::PCCERT_CONTEXT);

unsafe impl Sync for CertContext {}
unsafe impl Send for CertContext {}

impl Drop for CertContext {
    fn drop(&mut self) {
        unsafe {
            crypt32::CertFreeCertificateContext(self.0);
        }
    }
}

impl Clone for CertContext {
    fn clone(&self) -> CertContext {
        unsafe { CertContext(crypt32::CertDuplicateCertificateContext(self.0)) }
    }
}

inner!(CertContext, winapi::PCCERT_CONTEXT);

impl CertContext {
    /// Decodes a DER-formatted X509 certificate.
    pub fn new(data: &[u8]) -> io::Result<CertContext> {
        let ret = unsafe {
            crypt32::CertCreateCertificateContext(winapi::X509_ASN_ENCODING |
                                                  winapi::PKCS_7_ASN_ENCODING,
                                                  data.as_ptr(),
                                                  data.len() as winapi::DWORD)
        };
        if ret.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(CertContext(ret))
        }
    }

    /// Decodes a PEM-formatted X509 certificate.
    pub fn from_pem(pem: &str) -> io::Result<CertContext> {
        unsafe {
            assert!(pem.len() <= winapi::DWORD::max_value() as usize);

            let mut len = 0;
            let ok = crypt32::CryptStringToBinaryA(pem.as_ptr() as winapi::LPCSTR,
                                                   pem.len() as winapi::DWORD,
                                                   CRYPT_STRING_BASE64HEADER,
                                                   ptr::null_mut(),
                                                   &mut len,
                                                   ptr::null_mut(),
                                                   ptr::null_mut());
            if ok != winapi::TRUE {
                return Err(io::Error::last_os_error());
            }

            let mut buf = vec![0; len as usize];
            let ok = crypt32::CryptStringToBinaryA(pem.as_ptr() as winapi::LPCSTR,
                                                   pem.len() as winapi::DWORD,
                                                   CRYPT_STRING_BASE64HEADER,
                                                   buf.as_mut_ptr(),
                                                   &mut len,
                                                   ptr::null_mut(),
                                                   ptr::null_mut());
            if ok != winapi::TRUE {
                return Err(io::Error::last_os_error());
            }

            CertContext::new(&buf)
        }
    }

    /// Returns a hash of this certificate
    pub fn fingerprint(&self, alg: HashAlgorithm) -> io::Result<Vec<u8>> {
        unsafe {
            let mut buf = vec![0u8; alg.1];
            let mut len = buf.len() as winapi::DWORD;

            let ret = crypt32::CryptHashCertificate(0,
                                                    alg.0,
                                                    0,
                                                    (*self.0).pbCertEncoded,
                                                    (*self.0).cbCertEncoded,
                                                    buf.as_mut_ptr(),
                                                    &mut len);

            if ret != winapi::TRUE {
                return Err(io::Error::last_os_error());
            }
            Ok(buf)
        }
    }

    /// Returns the sha1 hash of this certificate
    ///
    /// The sha1 is returned as a 20-byte array representing the bits of the
    /// sha1 hash.
    #[deprecated(note = "please use fingerprint instead")]
    pub fn sha1(&self) -> io::Result<[u8; 20]> {
        let mut out = [0u8; 20];
        out.copy_from_slice(&try!(self.fingerprint(HashAlgorithm::sha1())));
        Ok(out)
    }

    /// Returns the `<SIGNATURE>/<HASH>` string representing the certificate
    /// signature.
    ///
    /// The `<SIGNATURE>` value identifies the CNG public key
    /// algorithm. The `<HASH>` value identifies the CNG hash algorithm.
    ///
    /// Common examples are:
    ///
    /// * `RSA/SHA1`
    /// * `RSA/SHA256`
    /// * `ECDSA/SHA256`
    pub fn sign_hash_algorithms(&self) -> io::Result<String> {
        self.get_string(winapi::CERT_SIGN_HASH_CNG_ALG_PROP_ID)
    }

    /// Returns the signature hash.
    pub fn signature_hash(&self) -> io::Result<Vec<u8>> {
        self.get_bytes(winapi::CERT_SIGNATURE_HASH_PROP_ID)
    }

    /// Returns the property displayed by the certificate UI. This property
    /// allows the user to describe the certificate's use.
    pub fn description(&self) -> io::Result<Vec<u8>> {
        self.get_bytes(winapi::CERT_DESCRIPTION_PROP_ID)
    }

    /// Returns a string that contains the display name for the certificate.
    pub fn friendly_name(&self) -> io::Result<String> {
        self.get_string(winapi::CERT_FRIENDLY_NAME_PROP_ID)
    }

    /// Configures the string that contains the display name for this
    /// certificate.
    pub fn set_friendly_name(&self, name: &str) -> io::Result<()> {
        self.set_string(winapi::CERT_FRIENDLY_NAME_PROP_ID, name)
    }

    /// Verifies the time validity of this certificate relative to the system's
    /// current time.
    pub fn is_time_valid(&self) -> io::Result<bool> {
        let ret = unsafe { crypt32::CertVerifyTimeValidity(ptr::null_mut(), (*self.0).pCertInfo) };
        Ok(ret == 0)
    }

    /// Returns a builder used to acquire the private key corresponding to this certificate.
    pub fn private_key<'a>(&'a self) -> AcquirePrivateKeyOptions<'a> {
        AcquirePrivateKeyOptions {
            cert: self,
            flags: 0,
        }
    }

    /// Deletes this certificate from its certificate store.
    pub fn delete(self) -> io::Result<()> {
        unsafe {
            let ret = crypt32::CertDeleteCertificateFromStore(self.0);
            mem::forget(self);
            if ret == winapi::TRUE {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
    }

    /// Returns a builder used to set the private key associated with this certificate.
    pub fn set_key_prov_info<'a>(&'a self) -> SetKeyProvInfo<'a> {
        SetKeyProvInfo {
            cert: self,
            container: None,
            provider: None,
            type_: 0,
            flags: 0,
            key_spec: 0,
        }
    }

    fn get_encoded_bytes(&self) -> &[u8] {
        unsafe {
            let cert_ctx = *self.0;
            slice::from_raw_parts(cert_ctx.pbCertEncoded, cert_ctx.cbCertEncoded as usize)
        }
    }

    fn get_bytes(&self, prop: winapi::DWORD) -> io::Result<Vec<u8>> {
        unsafe {
            let mut len = 0;
            let ret =
                crypt32::CertGetCertificateContextProperty(self.0, prop, ptr::null_mut(), &mut len);
            if ret != winapi::TRUE {
                return Err(io::Error::last_os_error());
            }

            let mut buf = vec![0u8; len as usize];
            let ret = crypt32::CertGetCertificateContextProperty(self.0,
                                                                 prop,
                                                                 buf.as_mut_ptr() as
                                                                 *mut winapi::c_void,
                                                                 &mut len);
            if ret != winapi::TRUE {
                return Err(io::Error::last_os_error());
            }
            Ok(buf)
        }
    }

    fn get_string(&self, prop: winapi::DWORD) -> io::Result<String> {
        unsafe {
            let mut len = 0;
            let ret =
                crypt32::CertGetCertificateContextProperty(self.0, prop, ptr::null_mut(), &mut len);
            if ret != winapi::TRUE {
                return Err(io::Error::last_os_error());
            }

            // Divide by 2 b/c `len` is the byte length, but we're allocating
            // u16 pairs which are 2 bytes each.
            let amt = (len / 2) as usize;
            let mut buf = vec![0u16; amt];
            let ret = crypt32::CertGetCertificateContextProperty(self.0,
                                                                 prop,
                                                                 buf.as_mut_ptr() as
                                                                 *mut winapi::c_void,
                                                                 &mut len);
            if ret != winapi::TRUE {
                return Err(io::Error::last_os_error());
            }

            // Chop off the trailing nul byte
            Ok(OsString::from_wide(&buf[..amt - 1]).into_string().unwrap())
        }
    }

    fn set_string(&self, prop: winapi::DWORD, s: &str) -> io::Result<()> {
        unsafe {
            let data = s.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
            let data = winapi::CRYPT_DATA_BLOB {
                cbData: (data.len() * 2) as winapi::DWORD,
                pbData: data.as_ptr() as *mut _,
            };
            let ret = crypt32::CertSetCertificateContextProperty(self.0,
                                                                 prop,
                                                                 0,
                                                                 &data as *const _ as *const _);
            if ret != winapi::TRUE {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }
}

impl PartialEq for CertContext {
    fn eq(&self, other: &CertContext) -> bool {
        self.get_encoded_bytes() == other.get_encoded_bytes()
    }
}

/// A builder type for certificate private key lookup.
pub struct AcquirePrivateKeyOptions<'a> {
    cert: &'a CertContext,
    flags: winapi::DWORD,
}

impl<'a> AcquirePrivateKeyOptions<'a> {
    /// If set, the certificate's public key will be compared with the private key to ensure a
    /// match.
    pub fn compare_key(&mut self, compare_key: bool) -> &mut AcquirePrivateKeyOptions<'a> {
        self.flag(CRYPT_ACQUIRE_COMPARE_KEY_FLAG, compare_key)
    }

    /// If set, the lookup will not display any user interface, even if that causes the lookup to
    /// fail.
    pub fn silent(&mut self, silent: bool) -> &mut AcquirePrivateKeyOptions<'a> {
        self.flag(CRYPT_ACQUIRE_SILENT_FLAG, silent)
    }

    fn flag(&mut self, flag: winapi::DWORD, set: bool) -> &mut AcquirePrivateKeyOptions<'a> {
        if set {
            self.flags |= flag;
        } else {
            self.flags &= !flag;
        }
        self
    }

    /// Acquires the private key handle.
    pub fn acquire(&self) -> io::Result<PrivateKey> {
        unsafe {
            let flags = self.flags | CRYPT_ACQUIRE_ALLOW_NCRYPT_KEY_FLAG;
            let mut handle = 0;
            let mut spec = 0;
            let mut free = winapi::FALSE;
            let res = crypt32::CryptAcquireCertificatePrivateKey(self.cert.0,
                                                                 flags,
                                                                 ptr::null_mut(),
                                                                 &mut handle,
                                                                 &mut spec,
                                                                 &mut free);
            if res != winapi::TRUE {
                return Err(io::Error::last_os_error());
            }
            assert!(free == winapi::TRUE);
            if spec & winapi::CERT_NCRYPT_KEY_SPEC != 0 {
                Ok(PrivateKey::NcryptKey(NcryptKey::from_inner(handle)))
            } else {
                Ok(PrivateKey::CryptProv(CryptProv::from_inner(handle)))
            }
        }
    }
}

/// The private key associated with a certificate context.
pub enum PrivateKey {
    /// A CryptoAPI provider.
    CryptProv(CryptProv),
    /// A CNG provider.
    NcryptKey(NcryptKey),
}

/// A builder used to set the private key associated with a certificate.
pub struct SetKeyProvInfo<'a> {
    cert: &'a CertContext,
    container: Option<Vec<u16>>,
    provider: Option<Vec<u16>>,
    type_: winapi::DWORD,
    flags: winapi::DWORD,
    key_spec: winapi::DWORD,
}

impl<'a> SetKeyProvInfo<'a> {
    /// The name of the key container.
    ///
    /// If `type_` is not provided, this specifies the name of the key withing
    /// the CNG key storage provider.
    pub fn container(&mut self, container: &str) -> &mut SetKeyProvInfo<'a> {
        self.container = Some(container.encode_utf16().chain(Some(0)).collect());
        self
    }

    /// The name of the CSP.
    ///
    /// If `type_` is not provided, this contains the name of the CNG key
    /// storage provider.
    pub fn provider(&mut self, provider: &str) -> &mut SetKeyProvInfo<'a> {
        self.provider = Some(provider.encode_utf16().chain(Some(0)).collect());
        self
    }

    /// Sets the CSP type.
    ///
    /// If not provided, the key container is one of the CNG key storage
    /// providers.
    pub fn type_(&mut self, type_: ProviderType) -> &mut SetKeyProvInfo<'a> {
        self.type_ = type_.as_raw();
        self
    }

    /// If set, the handle to the key provider can be kept open for subsequent
    /// calls to cryptographic functions.
    pub fn keep_open(&mut self, keep_open: bool) -> &mut SetKeyProvInfo<'a> {
        self.flag(winapi::CERT_SET_KEY_PROV_HANDLE_PROP_ID, keep_open)
    }

    /// If set, the key container contains machine keys.
    pub fn machine_keyset(&mut self, machine_keyset: bool) -> &mut SetKeyProvInfo<'a> {
        self.flag(winapi::CRYPT_MACHINE_KEYSET, machine_keyset)
    }

    /// If set, the key container will attempt to open keys without any user
    /// interface prompts.
    pub fn silent(&mut self, silent: bool) -> &mut SetKeyProvInfo<'a> {
        self.flag(winapi::CRYPT_SILENT, silent)
    }

    fn flag(&mut self, flag: winapi::DWORD, on: bool) -> &mut SetKeyProvInfo<'a> {
        if on {
            self.flags |= flag;
        } else {
            self.flags &= !flag;
        }
        self
    }

    /// The specification of the private key to retrieve.
    pub fn key_spec(&mut self, key_spec: KeySpec) -> &mut SetKeyProvInfo<'a> {
        self.key_spec = key_spec.0;
        self
    }

    /// Sets the private key for this certificate.
    pub fn set(&mut self) -> io::Result<()> {
        unsafe {
            let container = self.container.as_ref().map(|s| s.as_ptr()).unwrap_or(ptr::null());
            let provider = self.provider.as_ref().map(|s| s.as_ptr()).unwrap_or(ptr::null());

            let info = winapi::CRYPT_KEY_PROV_INFO {
                pwszContainerName: container as *mut _,
                pwszProvName: provider as *mut _,
                dwProvType: self.type_,
                dwFlags: self.flags,
                cProvParam: 0,
                rgProvParam: ptr::null_mut(),
                dwKeySpec: self.key_spec,
            };

            let res =
                crypt32::CertSetCertificateContextProperty(self.cert.0,
                                                           winapi::CERT_KEY_PROV_INFO_PROP_ID,
                                                           0,
                                                           &info as *const _ as *const _);
            if res == winapi::TRUE {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
    }
}

/// The specification of a private key.
#[derive(Copy, Clone)]
pub struct KeySpec(winapi::DWORD);

impl KeySpec {
    /// A key used to encrypt/decrypt session keys.
    pub fn key_exchange() -> KeySpec {
        KeySpec(winapi::AT_KEYEXCHANGE)
    }

    /// A key used to create and verify digital signatures.
    pub fn signature() -> KeySpec {
        KeySpec(winapi::AT_SIGNATURE)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn decode() {
        let der = include_bytes!("../test/cert.der");
        let pem = include_str!("../test/cert.pem");

        let der = CertContext::new(der).unwrap();
        let pem = CertContext::from_pem(pem).unwrap();
        assert_eq!(der, pem);
    }

    #[test]
    fn fingerprint() {
        let der = include_bytes!("../test/cert.der");
        let pem = include_str!("../test/cert.pem");

        let der = CertContext::new(der).unwrap();
        let pem = CertContext::from_pem(pem).unwrap();

        let hash = der.fingerprint(HashAlgorithm::sha1()).unwrap();
        assert_eq!(hash, vec![‎
            0x59, 0x17, 0x2D, 0x93, 0x13, 0xE8, 0x44, 0x59, 0xBC, 0xFF,
            0x27, 0xF9, 0x67, 0xE7, 0x9E, 0x6E, 0x92, 0x17, 0xE5, 0x84
        ]);
        assert_eq!(hash, pem.fingerprint(HashAlgorithm::sha1()).unwrap());

        let hash = der.fingerprint(HashAlgorithm::sha256()).unwrap();
        assert_eq!(hash, vec![
            0x47, 0x12, 0xB9, 0x39, 0xFB, 0xCB, 0x42, 0xA6, 0xB5, 0x10,
            0x1B, 0x42, 0x13, 0x9A, 0x25, 0xB1, 0x4F, 0x81, 0xB4, 0x18,
            0xFA, 0xCA, 0xBD, 0x37, 0x87, 0x46, 0xF1, 0x2F, 0x85, 0xCC,
            0x65, 0x44
        ]);
        assert_eq!(hash, pem.fingerprint(HashAlgorithm::sha256()).unwrap());
    }
}
