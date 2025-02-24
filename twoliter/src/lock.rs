use crate::common::fs::{create_dir_all, read, remove_dir_all, write};
use crate::project::{Image, Project, ValidIdentifier, Vendor};
use crate::schema_version::SchemaVersion;
use anyhow::{bail, ensure, Context, Result};
use base64::Engine;
use futures::pin_mut;
use futures::stream::{self, StreamExt, TryStreamExt};
use oci_cli_wrapper::{DockerArchitecture, ImageTool};
use olpc_cjson::CanonicalFormatter as CanonicalJsonFormatter;
use semver::Version;
use serde::de::Error;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::Digest;
use std::cmp::PartialEq;
use std::collections::{HashMap, HashSet};
use std::fmt::{Debug, Display, Formatter};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::mem::take;
use std::path::{Path, PathBuf};
use tar::Archive as TarArchive;
use tokio::fs::read_to_string;
use tracing::{debug, error, info, instrument, trace};

const TWOLITER_LOCK: &str = "Twoliter.lock";

/// Represents a locked dependency on an image
#[derive(Debug, Clone, Eq, Ord, PartialOrd, Serialize, Deserialize)]
pub(crate) struct LockedImage {
    /// The name of the dependency
    pub name: String,
    /// The version of the dependency
    pub version: Version,
    /// The vendor this dependency came from
    pub vendor: String,
    /// The resolved image uri of the dependency
    pub source: String,
    /// The digest of the image
    pub digest: String,
    #[serde(skip)]
    pub(crate) manifest: Vec<u8>,
}

impl PartialEq for LockedImage {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source && self.digest == other.digest
    }
}

impl LockedImage {
    pub async fn new(image_tool: &ImageTool, vendor: &Vendor, image: &Image) -> Result<Self> {
        let source = format!("{}/{}:v{}", vendor.registry, image.name, image.version);
        debug!("Pulling image manifest for locked image '{}'", source);
        let manifest_bytes = image_tool.get_manifest(source.as_str()).await?;

        // We calculate a 'digest' of the manifest to use as our unique id
        let digest = sha2::Sha256::digest(manifest_bytes.as_slice());
        let digest = base64::engine::general_purpose::STANDARD.encode(digest.as_slice());
        trace!(
            "Calculated digest for locked image '{}': '{}'",
            source,
            digest
        );

        Ok(Self {
            name: image.name.to_string(),
            version: image.version.clone(),
            vendor: image.vendor.to_string(),
            source,
            digest,
            manifest: manifest_bytes,
        })
    }

    pub fn digest_uri(&self, digest: &str) -> String {
        self.source.replace(
            format!(":v{}", self.version).as_str(),
            format!("@{}", digest).as_str(),
        )
    }
}

impl Display for LockedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "{}-{}@{} ({})",
            self.name, self.version, self.vendor, self.source,
        ))
    }
}

/// The hash should not contain the source to allow for collision detection
impl Hash for LockedImage {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.version.hash(state);
        self.vendor.hash(state);
    }
}

#[derive(Deserialize, Debug, Clone)]
struct ImageMetadata {
    /// The name of the kit
    #[allow(dead_code)]
    pub name: String,
    /// The version of the kit
    #[allow(dead_code)]
    pub version: Version,
    /// The required sdk of the kit,
    pub sdk: Image,
    /// Any dependent kits
    #[serde(rename = "kit")]
    pub kits: Vec<Image>,
}

impl TryFrom<EncodedKitMetadata> for ImageMetadata {
    type Error = anyhow::Error;

    fn try_from(value: EncodedKitMetadata) -> Result<Self, Self::Error> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(value.0)
            .context("failed to decode kit metadata as base64")?;
        serde_json::from_slice(bytes.as_slice()).context("failed to parse kit metadata json")
    }
}

/// Encoded kit metadata, which is embedded in a label of the OCI image config.
#[derive(Clone, Eq, PartialEq)]
struct EncodedKitMetadata(String);

impl EncodedKitMetadata {
    #[instrument(level = "trace")]
    async fn try_from_image(image_uri: &str, image_tool: &ImageTool) -> Result<Self> {
        trace!(image_uri, "Extracting kit metadata from OCI image config");
        let config = image_tool.get_config(image_uri).await?;
        let kit_metadata = EncodedKitMetadata(
            config
                .labels
                .get("dev.bottlerocket.kit.v1")
                .context("no metadata stored on image, this image appears to not be a kit")?
                .to_owned(),
        );

        trace!(
            image_uri,
            image_config = ?config,
            ?kit_metadata,
            "Kit metadata retrieved from image config"
        );

        Ok(kit_metadata)
    }

    /// Infallible method to provide debugging insights into encoded `ImageMetadata`
    ///
    /// Shows a `Debug` view of the encoded `ImageMetadata` if possible, otherwise shows
    /// the encoded form.
    fn try_debug_image_metadata(&self) -> String {
        self.debug_image_metadata().unwrap_or_else(|| {
            format!("<ImageMetadata(encoded) [{}]>", self.0.replace("\n", "\\n"))
        })
    }

    fn debug_image_metadata(&self) -> Option<String> {
        base64::engine::general_purpose::STANDARD
            .decode(&self.0)
            .ok()
            .and_then(|bytes| serde_json::from_slice(bytes.as_slice()).ok())
            .map(|metadata: ImageMetadata| format!("<ImageMetadata(decoded) [{:?}]>", metadata))
    }
}

impl Debug for EncodedKitMetadata {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.try_debug_image_metadata())
    }
}

#[derive(Deserialize, Debug)]
struct ManifestListView {
    manifests: Vec<ManifestView>,
}

#[derive(Deserialize, Debug, Clone)]
struct ManifestView {
    digest: String,
    platform: Option<Platform>,
}

#[derive(Deserialize, Debug, Clone)]
struct Platform {
    architecture: DockerArchitecture,
}

#[derive(Deserialize, Debug)]
struct IndexView {
    manifests: Vec<ManifestView>,
}

#[derive(Deserialize, Debug)]
struct ManifestLayoutView {
    layers: Vec<Layer>,
}

#[derive(Deserialize, Debug)]
struct Layer {
    digest: ContainerDigest,
}

#[derive(Debug)]
struct ContainerDigest(String);

impl<'de> Deserialize<'de> for ContainerDigest {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let digest = String::deserialize(deserializer)?;
        if !digest.starts_with("sha256:") {
            return Err(D::Error::custom(format!(
                "invalid digest detected in layer: {}",
                digest
            )));
        };
        Ok(Self(digest))
    }
}

impl Display for ContainerDigest {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.as_str())
    }
}

#[derive(Serialize, Debug)]
struct ExternalKitMetadata {
    sdk: LockedImage,
    #[serde(rename = "kit")]
    kits: Vec<LockedImage>,
}

#[derive(Debug)]
struct OCIArchive {
    image: LockedImage,
    digest: String,
    cache_dir: PathBuf,
}

impl OCIArchive {
    fn new<P>(image: &LockedImage, digest: &str, cache_dir: P) -> Result<Self>
    where
        P: AsRef<Path>,
    {
        Ok(Self {
            image: image.clone(),
            digest: digest.into(),
            cache_dir: cache_dir.as_ref().to_path_buf(),
        })
    }

    fn archive_path(&self) -> PathBuf {
        self.cache_dir.join(self.digest.replace(':', "-"))
    }

    #[instrument(level = "trace", skip_all, fields(image = %self.image))]
    async fn pull_image(&self, image_tool: &ImageTool) -> Result<()> {
        debug!("Pulling image '{}'", self.image);
        let digest_uri = self.image.digest_uri(self.digest.as_str());
        let oci_archive_path = self.archive_path();
        if !oci_archive_path.exists() {
            create_dir_all(&oci_archive_path).await?;
            image_tool
                .pull_oci_image(oci_archive_path.as_path(), digest_uri.as_str())
                .await?;
        } else {
            debug!("Image '{}' already present -- no need to pull.", self.image);
        }
        Ok(())
    }

    #[instrument(
        level = "trace",
        skip_all,
        fields(image = %self.image, out_dir = %out_dir.as_ref().display()),
    )]
    async fn unpack_layers<P>(&self, out_dir: P) -> Result<()>
    where
        P: AsRef<Path>,
    {
        let path = out_dir.as_ref();
        let digest_file = path.join("digest");
        if digest_file.exists() {
            let digest = read_to_string(&digest_file).await.context(format!(
                "failed to read digest file at {}",
                digest_file.display()
            ))?;
            if digest == self.digest {
                trace!(
                    "Found existing digest file for image '{}' at '{}'",
                    self.image,
                    digest_file.display()
                );
                return Ok(());
            }
        }

        debug!("Unpacking layers for image '{}'", self.image);
        remove_dir_all(path).await?;
        create_dir_all(path).await?;
        let index_bytes = read(self.archive_path().join("index.json")).await?;
        let index: IndexView = serde_json::from_slice(index_bytes.as_slice())
            .context("failed to deserialize oci image index")?;

        // Read the manifest so we can get the layer digests
        trace!(image = %self.image, "Extracting layer digests from image manifest");
        let digest = index
            .manifests
            .first()
            .context("empty oci image")?
            .digest
            .replace(':', "/");
        let manifest_bytes = read(self.archive_path().join(format!("blobs/{digest}")))
            .await
            .context("failed to read manifest blob")?;
        let manifest_layout: ManifestLayoutView = serde_json::from_slice(manifest_bytes.as_slice())
            .context("failed to deserialize oci manifest")?;

        // Extract each layer into the target directory
        trace!(image = %self.image, "Extracting image layers");
        for layer in manifest_layout.layers {
            let digest = layer.digest.to_string().replace(':', "/");
            let layer_blob = File::open(self.archive_path().join(format!("blobs/{digest}")))
                .context("failed to read layer of oci image")?;
            let mut layer_archive = TarArchive::new(layer_blob);
            layer_archive
                .unpack(path)
                .context("failed to unpack layer to disk")?;
        }
        write(&digest_file, self.digest.as_str())
            .await
            .context(format!(
                "failed to record digest to {}",
                digest_file.display()
            ))?;

        Ok(())
    }
}

/// Represents the structure of a `Twoliter.lock` lock file.
#[derive(Debug, Clone, PartialEq, Eq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct Lock {
    /// The version of the Twoliter.toml this was generated from
    pub schema_version: SchemaVersion<1>,
    /// The resolved bottlerocket sdk
    pub sdk: LockedImage,
    /// Resolved kit dependencies
    pub kit: Vec<LockedImage>,
}

#[allow(dead_code)]
impl Lock {
    #[instrument(level = "trace", skip(project))]
    pub(crate) async fn create(project: &Project) -> Result<Self> {
        let lock_file_path = project.project_dir().join(TWOLITER_LOCK);

        info!("Resolving project references to create lock file");
        let lock_state = Self::resolve(project).await?;
        let lock_str = toml::to_string(&lock_state).context("failed to serialize lock file")?;

        debug!("Writing new lock file to '{}'", lock_file_path.display());
        write(&lock_file_path, lock_str)
            .await
            .context("failed to write lock file")?;
        Ok(lock_state)
    }

    #[instrument(level = "trace", skip(project))]
    pub(crate) async fn load(project: &Project) -> Result<Self> {
        let lock_file_path = project.project_dir().join(TWOLITER_LOCK);
        ensure!(
            lock_file_path.exists(),
            "Twoliter.lock does not exist, please run `twoliter update` first"
        );
        debug!("Loading existing lockfile '{}'", lock_file_path.display());
        let lock_str = read_to_string(&lock_file_path)
            .await
            .context("failed to read lockfile")?;
        let lock: Self =
            toml::from_str(lock_str.as_str()).context("failed to deserialize lockfile")?;

        info!("Resolving project references to check against lock file");
        let lock_state = Self::resolve(project).await?;

        ensure!(lock_state == lock, "changes have occured to Twoliter.toml or the remote kit images that require an update to Twoliter.lock");
        Ok(lock)
    }

    fn external_kit_metadata(&self) -> ExternalKitMetadata {
        ExternalKitMetadata {
            sdk: self.sdk.clone(),
            kits: self.kit.clone(),
        }
    }

    /// Fetches all external kits defined in a Twoliter.lock to the build directory
    #[instrument(level = "trace", skip_all)]
    pub(crate) async fn fetch(&self, project: &Project, arch: &str) -> Result<()> {
        let image_tool = ImageTool::from_environment()?;
        let target_dir = project.external_kits_dir();
        create_dir_all(&target_dir).await.context(format!(
            "failed to create external-kits directory at {}",
            target_dir.display()
        ))?;

        info!(
            dependencies = ?self.kit.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "Extracting kit dependencies."
        );
        for image in self.kit.iter() {
            self.extract_kit(&image_tool, &project.external_kits_dir(), image, arch)
                .await?;
        }
        let mut kit_list = Vec::new();
        let mut ser =
            serde_json::Serializer::with_formatter(&mut kit_list, CanonicalJsonFormatter::new());
        self.external_kit_metadata()
            .serialize(&mut ser)
            .context("failed to serialize external kit metadata")?;
        // Compare the output of the serialize if the file exists
        let external_metadata_file = project.external_kits_metadata();
        if external_metadata_file.exists() {
            let existing = read(&external_metadata_file).await.context(format!(
                "failed to read external kit metadata: {}",
                external_metadata_file.display()
            ))?;
            // If this is the same as what we generated skip the write
            if existing == kit_list {
                return Ok(());
            }
        }
        write(project.external_kits_metadata(), kit_list.as_slice())
            .await
            .context(format!(
                "failed to write external kit metadata: {}",
                project.external_kits_metadata().display()
            ))?;

        Ok(())
    }

    #[instrument(level = "trace", skip(image), fields(image = %image))]
    async fn get_manifest(
        &self,
        image_tool: &ImageTool,
        image: &LockedImage,
        arch: &str,
    ) -> Result<ManifestView> {
        let manifest_bytes = image_tool.get_manifest(image.source.as_str()).await?;
        let manifest_list: ManifestListView = serde_json::from_slice(manifest_bytes.as_slice())
            .context("failed to deserialize manifest list")?;
        let docker_arch = DockerArchitecture::try_from(arch)?;
        manifest_list
            .manifests
            .iter()
            .find(|x| x.platform.as_ref().unwrap().architecture == docker_arch)
            .cloned()
            .context(format!(
                "could not find kit image for architecture '{}' at {}",
                docker_arch, image.source
            ))
    }

    #[instrument(
        level = "trace",
        skip(image),
        fields(image = %image, path = %path.as_ref().display())
    )]
    async fn extract_kit<P>(
        &self,
        image_tool: &ImageTool,
        path: P,
        image: &LockedImage,
        arch: &str,
    ) -> Result<()>
    where
        P: AsRef<Path>,
    {
        info!(
            "Extracting kit '{}' to '{}'",
            image,
            path.as_ref().display()
        );
        let vendor = image.vendor.clone();
        let name = image.name.clone();
        let target_path = path.as_ref().join(format!("{vendor}/{name}/{arch}"));
        let cache_path = path.as_ref().join("cache");
        create_dir_all(&target_path).await?;
        create_dir_all(&cache_path).await?;

        // First get the manifest for the specific requested architecture
        let manifest = self.get_manifest(image_tool, image, arch).await?;
        let oci_archive = OCIArchive::new(image, manifest.digest.as_str(), &cache_path)?;

        // Checks for the saved image locally, or else pulls and saves it
        oci_archive.pull_image(image_tool).await?;

        // Checks if this archive has already been extracted by checking a digest file
        // otherwise cleans up the path and unpacks the archive
        oci_archive.unpack_layers(&target_path).await?;

        Ok(())
    }

    #[instrument(level = "trace", skip(project))]
    async fn resolve(project: &Project) -> Result<Self> {
        let vendor_table = project.vendor();
        let mut known: HashMap<(ValidIdentifier, ValidIdentifier), Version> = HashMap::new();
        let mut locked: Vec<LockedImage> = Vec::new();
        let image_tool = ImageTool::from_environment()?;

        let mut remaining: Vec<Image> = project.kits();
        let mut sdk_set: HashSet<Image> = HashSet::new();
        if let Some(sdk) = project.sdk_image() {
            // We don't scan over the sdk images as they are not kit images and there is no kit metadata to fetch
            sdk_set.insert(sdk.clone());
        }
        while !remaining.is_empty() {
            let working_set: Vec<_> = take(&mut remaining);
            for image in working_set.iter() {
                debug!(%image, "Resolving kit '{}'", image.name);
                if let Some(version) = known.get(&(image.name.clone(), image.vendor.clone())) {
                    let name = image.name.clone();
                    let left_version = image.version.clone();
                    let vendor = image.vendor.clone();
                    ensure!(
                        image.version == *version,
                        "cannot have multiple versions of the same kit ({name}-{left_version}@{vendor} != {name}-{version}@{vendor}",
                    );
                    debug!(
                        ?image,
                        "Skipping kit '{}' as it has already been resolved", image.name
                    );
                    continue;
                }
                let vendor = vendor_table.get(&image.vendor).context(format!(
                    "vendor '{}' is not specified in Twoliter.toml",
                    image.vendor
                ))?;
                known.insert(
                    (image.name.clone(), image.vendor.clone()),
                    image.version.clone(),
                );
                let locked_image = LockedImage::new(&image_tool, vendor, image).await?;
                let kit = Self::find_kit(&image_tool, vendor, &locked_image).await?;
                locked.push(locked_image);
                sdk_set.insert(kit.sdk);
                for dep in kit.kits {
                    remaining.push(dep);
                }
            }
        }

        debug!(?sdk_set, "Resolving workspace SDK");
        ensure!(
            sdk_set.len() <= 1,
            "cannot use multiple sdks (found sdk: {})",
            sdk_set
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
        let sdk = sdk_set
            .iter()
            .next()
            .context("no sdk was found for use, please specify a sdk in Twoliter.toml")?;
        let vendor = vendor_table.get(&sdk.vendor).context(format!(
            "vendor '{}' is not specified in Twoliter.toml",
            sdk.vendor
        ))?;
        Ok(Self {
            schema_version: project.schema_version(),
            sdk: LockedImage::new(&image_tool, vendor, sdk).await?,
            kit: locked,
        })
    }

    #[instrument(level = "trace", skip(image), fields(image = %image))]
    async fn find_kit(
        image_tool: &ImageTool,
        vendor: &Vendor,
        image: &LockedImage,
    ) -> Result<ImageMetadata> {
        debug!(kit_image = %image, "Searching for kit");
        let manifest_list: ManifestListView = serde_json::from_slice(image.manifest.as_slice())
            .context("failed to deserialize manifest list")?;
        trace!(manifest_list = ?manifest_list, "Deserialized manifest list");
        debug!("Extracting kit metadata from OCI image");
        let embedded_kit_metadata =
            stream::iter(manifest_list.manifests).then(|manifest| async move {
                let image_uri = format!("{}/{}@{}", vendor.registry, image.name, manifest.digest);
                EncodedKitMetadata::try_from_image(&image_uri, image_tool).await
            });
        pin_mut!(embedded_kit_metadata);

        let canonical_metadata = embedded_kit_metadata
            .try_next()
            .await?
            .context(format!("could not find metadata for kit {}", image))?;

        trace!("Checking that all manifests refer to the same kit.");
        while let Some(kit_metadata) = embedded_kit_metadata.try_next().await? {
            if kit_metadata != canonical_metadata {
                error!(
                    ?canonical_metadata,
                    ?kit_metadata,
                    "Mismatched kit metadata in manifest list"
                );
                bail!("Metadata does not match between images in manifest list");
            }
        }

        canonical_metadata
            .try_into()
            .context("Failed to decode and parse kit metadata")
    }
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn test_try_debug_image_metadata_succeeds() {
        // Given a valid encoded metadata string,
        // When we attempt to decode it for debugging,
        // Then the debug string is marked as having been decoded.
        let encoded = EncodedKitMetadata(
            "eyJraXQiOltdLCJuYW1lIjoiYm90dGxlcm9ja2V0LWNvcmUta2l0Iiwic2RrIjp7ImRpZ2VzdCI6ImlyY09EUl\
            d3ZmxjTTdzaisrMmszSk5RWkovb3ZDUVRpUlkrRFpvaGdrNlk9IiwibmFtZSI6InRoYXItYmUtYmV0YS1zZGsiL\
            CJzb3VyY2UiOiJwdWJsaWMuZWNyLmF3cy91MWczYzh6NC90aGFyLWJlLWJldGEtc2RrOnYwLjQzLjAiLCJ2ZW5k\
            b3IiOiJib3R0bGVyb2NrZXQtbmV3IiwidmVyc2lvbiI6IjAuNDMuMCJ9LCJ2ZXJzaW9uIjoiMi4wLjAifQo="
            .to_string()
        );
        assert!(encoded.debug_image_metadata().is_some());
    }

    #[test]
    fn test_try_debug_image_metadata_fails() {
        // Given an invalid encoded metadata string,
        // When we attempt to decode it for debugging,
        // Then the debug string is marked as remaining encoded.
        let junk_data = EncodedKitMetadata("abcdefghijklmnophello".to_string());
        assert!(junk_data.debug_image_metadata().is_none());
    }
}
