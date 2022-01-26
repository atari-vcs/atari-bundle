use std::fmt::{self, Display, Formatter};
use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::path::Path;
use std::str::FromStr;

use itertools::Itertools;
use log::error;
use serde::de::{Error as DeserError, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_with::{DeserializeFromStr, SerializeDisplay};
use strum::{Display, EnumString};
use thiserror::Error;
use zip::{read::ZipArchive, result::ZipError, write::ZipWriter};

fn is_false(b: &bool) -> bool {
    !b
}

#[derive(Debug, Error)]
pub enum BundleError {
    #[error("error opening zipfile: {0}")]
    Zip(#[from] ZipError),
    #[error("bad config file: {0}")]
    De(#[from] serde_ini::de::Error),
    #[error("io error reading bundle: {0}")]
    Io(#[from] io::Error),
    #[error("unable to write config file: {0}")]
    Ser(#[from] serde_ini::ser::Error),
}

pub type BundleResult<T> = Result<T, BundleError>;

#[derive(Clone, Copy, Debug, Display, EnumString, DeserializeFromStr, SerializeDisplay)]
pub enum BundleType {
    Game,
    Application,
    LauncherOnly,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
pub struct Bundle {
    pub name: String,
    #[serde(rename = "Type")]
    pub bundle_type: BundleType,
    #[serde(rename = "StoreID", skip_serializing_if = "Option::is_none")]
    pub store_id: Option<String>,
    #[serde(rename = "HomebrewID", skip_serializing_if = "Option::is_none")]
    pub homebrew_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(
        default,
        serialize_with = "ser_keyfile_bool",
        deserialize_with = "de_keyfile_bool",
        skip_serializing_if = "is_false"
    )]
    pub background: bool,
    #[serde(
        rename = "PreferXBoxMode",
        default,
        serialize_with = "ser_keyfile_bool",
        deserialize_with = "de_keyfile_bool",
        skip_serializing_if = "is_false"
    )]
    pub prefer_xbox_mode: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub launcher: Option<String>,
    #[serde(
        default,
        deserialize_with = "de_keyfile_list",
        serialize_with = "ser_keyfile_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub launcher_tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launcher_exec: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct BundleConfig {
    pub bundle: Bundle,
}

// serde_ini won't serialize bools
fn ser_keyfile_bool<S>(value: &bool, ser: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    ser.serialize_str(if *value { "true" } else { "false" })
}

fn de_keyfile_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    struct BoolVisitor;

    impl<'de> Visitor<'de> for BoolVisitor {
        type Value = bool;

        fn expecting(&self, formatter: &mut Formatter) -> fmt::Result {
            write!(formatter, "Boolean")
        }

        fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
        where
            E: DeserError,
        {
            match s {
                "true" => Ok(true),
                "false" => Ok(false),
                _ => Err(DeserError::custom("not a valid Boolean value")),
            }
        }
    }

    deserializer.deserialize_str(BoolVisitor)
}

fn ser_keyfile_list<T, S>(value: &[T], ser: S) -> Result<S::Ok, S::Error>
where
    T: Display,
    S: Serializer,
{
    ser.serialize_str(
        Itertools::intersperse(value.iter().map(T::to_string), ";".to_string())
            .collect::<String>()
            .as_str(),
    )
}

fn de_keyfile_list<'de, T, D>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    T: FromStr,
    <T as FromStr>::Err: Display,
    D: Deserializer<'de>,
{
    struct SemicolonSeparatedVisitor<T>(std::marker::PhantomData<T>);

    impl<'de, T> Visitor<'de> for SemicolonSeparatedVisitor<T>
    where
        T: FromStr,
        <T as FromStr>::Err: Display,
    {
        type Value = Vec<T>;

        fn expecting(&self, formatter: &mut Formatter) -> fmt::Result {
            write!(formatter, "semicolon separated list")
        }

        fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
        where
            E: DeserError,
        {
            let mut v = s.split(';').collect::<Vec<_>>();
            if let Some(tail) = v.last() {
                if tail.is_empty() {
                    v.pop();
                }
            }

            v.into_iter()
                .map(FromStr::from_str)
                .collect::<Result<Vec<_>, <T as FromStr>::Err>>()
                .map_err(DeserError::custom)
        }
    }

    deserializer.deserialize_str(SemicolonSeparatedVisitor(Default::default()))
}

pub struct BundleConfigBuilder {
    name: String,
    bundle_type: BundleType,
}

impl BundleConfigBuilder {
    pub fn new(name: String, bundle_type: BundleType) -> Self {
        Self { name, bundle_type }
    }

    pub fn homebrew_id(self, id: String) -> HomebrewBundleConfigBuilder {
        HomebrewBundleConfigBuilder {
            name: self.name,
            bundle_type: self.bundle_type,
            exec: None,
            homebrew_id: id,
            launcher: None,
            prefer_xbox_mode: false,
            version: None,
        }
    }

    pub fn store_id(self, id: String) -> StoreBundleConfigBuilder {
        StoreBundleConfigBuilder {
            name: self.name,
            bundle_type: self.bundle_type,
            exec: None,
            store_id: id,
            launcher: None,
            launcher_exec: None,
            launcher_tags: Vec::new(),
            background: false,
            prefer_xbox_mode: false,
            version: None,
            encrypted_image: None,
        }
    }
}

pub struct HomebrewBundleConfigBuilder {
    name: String,
    bundle_type: BundleType,
    homebrew_id: String,

    exec: Option<String>,
    version: Option<String>,
    prefer_xbox_mode: bool,
    launcher: Option<String>,
}

impl HomebrewBundleConfigBuilder {
    pub fn version(mut self, version: String) -> Self {
        self.version = Some(version);
        self
    }

    pub fn prefer_xbox_mode(mut self, prefer_xbox_mode: bool) -> Self {
        self.prefer_xbox_mode = prefer_xbox_mode;
        self
    }

    pub fn requires_launcher(mut self, launcher: String) -> Self {
        self.launcher = Some(launcher);
        self
    }

    pub fn exec(mut self, exec: String) -> Self {
        self.exec = Some(exec);
        self
    }

    pub fn set_version(&mut self, version: Option<String>) -> &mut Self {
        self.version = version;
        self
    }

    pub fn set_prefer_xbox_mode(&mut self, prefer_xbox_mode: Option<bool>) -> &mut Self {
        self.prefer_xbox_mode = prefer_xbox_mode.unwrap_or_default();
        self
    }

    pub fn set_requires_launcher(&mut self, launcher: Option<String>) -> &mut Self {
        self.launcher = launcher;
        self
    }

    pub fn set_exec(&mut self, exec: Option<String>) -> &mut Self {
        self.exec = exec;
        self
    }

    pub fn build(self) -> BundleConfig {
        BundleConfig {
            bundle: Bundle {
                name: self.name,
                bundle_type: self.bundle_type,
                store_id: None,
                homebrew_id: Some(self.homebrew_id),
                exec: self.exec,
                version: self.version,
                background: false,
                prefer_xbox_mode: self.prefer_xbox_mode,
                launcher: self.launcher,
                launcher_tags: Vec::new(),
                launcher_exec: None,
                encrypted_image: None,
            },
        }
    }
}

pub struct StoreBundleConfigBuilder {
    name: String,
    bundle_type: BundleType,
    store_id: String,

    exec: Option<String>,
    version: Option<String>,
    background: bool,
    prefer_xbox_mode: bool,
    launcher: Option<String>,
    launcher_tags: Vec<String>,
    launcher_exec: Option<String>,
    encrypted_image: Option<String>,
}

impl StoreBundleConfigBuilder {
    pub fn version(mut self, version: String) -> Self {
        self.version = Some(version);
        self
    }

    pub fn background(mut self, background: bool) -> Self {
        self.background = background;
        self
    }

    pub fn prefer_xbox_mode(mut self, prefer_xbox_mode: bool) -> Self {
        self.prefer_xbox_mode = prefer_xbox_mode;
        self
    }

    pub fn requires_launcher(mut self, launcher: String) -> Self {
        self.launcher = Some(launcher);
        self
    }

    pub fn provides_launcher(mut self, exec: String, tags: Vec<String>) -> Self {
        self.launcher_exec = Some(exec);
        self.launcher_tags = tags;
        self
    }

    pub fn exec(mut self, exec: String) -> Self {
        self.exec = Some(exec);
        self
    }

    /// Set the store bundle config builder's encrypted image.
    pub fn encrypted_image(&mut self, encrypted_image: String) -> &mut Self {
        self.encrypted_image = Some(encrypted_image);
        self
    }

    pub fn set_version(&mut self, version: Option<String>) -> &mut Self {
        self.version = version;
        self
    }

    pub fn set_background(&mut self, background: Option<bool>) -> &mut Self {
        self.background = background.unwrap_or_default();
        self
    }

    pub fn set_prefer_xbox_mode(&mut self, prefer_xbox_mode: Option<bool>) -> &mut Self {
        self.prefer_xbox_mode = prefer_xbox_mode.unwrap_or_default();
        self
    }

    pub fn set_requires_launcher(&mut self, launcher: Option<String>) -> &mut Self {
        self.launcher = launcher;
        self
    }

    pub fn set_provides_launcher(&mut self, exec: Option<String>, tags: Vec<String>) -> &mut Self {
        if let Some(exe) = exec {
            self.launcher_exec = Some(exe);
            self.launcher_tags = tags;
        }
        self
    }

    pub fn set_exec(&mut self, exec: Option<String>) -> &mut Self {
        self.exec = exec;
        self
    }

    pub fn build(self) -> BundleConfig {
        BundleConfig {
            bundle: Bundle {
                name: self.name,
                bundle_type: self.bundle_type,
                store_id: Some(self.store_id),
                homebrew_id: None,
                exec: self.exec,
                version: self.version,
                background: self.background,
                prefer_xbox_mode: self.prefer_xbox_mode,
                launcher: self.launcher,
                launcher_tags: self.launcher_tags,
                launcher_exec: self.launcher_exec,
                encrypted_image: self.encrypted_image,
            },
        }
    }
}

impl BundleConfig {
    pub fn builder(name: String, bundle_type: BundleType) -> BundleConfigBuilder {
        BundleConfigBuilder::new(name, bundle_type)
    }

    pub fn from_read<R: Read>(read: R) -> BundleResult<Self> {
        Ok(serde_ini::from_read(read)?)
    }

    pub fn to_write<W: Write>(&self, write: W) -> BundleResult<()> {
        Ok(serde_ini::to_writer(write, self)?)
    }

    pub fn from_zipfile<P: AsRef<Path>>(path: P) -> BundleResult<Self> {
        let file = File::open(path.as_ref())?;
        let mut archive = ZipArchive::new(file)?;
        Self::from_archive(&mut archive)
    }

    pub fn from_archive<R: Read + Seek>(archive: &mut ZipArchive<R>) -> BundleResult<Self> {
        let inifile = archive.by_name("bundle.ini")?;
        Self::from_read(inifile)
    }

    pub fn to_archive<W: Write + Seek>(&self, writer: &mut ZipWriter<W>) -> BundleResult<()> {
        let options = zip::write::FileOptions::default();
        writer.start_file("bundle.ini", options)?;
        self.to_write(writer)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_deserialize_full_store() {
        let input = r#"
[Bundle]
Name=Test Name With Spaces
Type=Game
Exec=TestStoreID.exe
StoreID=TestStoreID
Version=10213 124213 sfsd alpha
Background=true
PreferXBoxMode=true
Launcher=wombat
LauncherTags=Tag1;Tag2;Tag3;Tag4
LauncherExec=TestStoreID_Launcher.exe
"#;
        let conf: BundleConfig =
            serde_ini::from_str(input).expect("failed to deserialize test input");
        assert_eq!(conf.bundle.name, "Test Name With Spaces");
        assert!(matches!(conf.bundle.bundle_type, BundleType::Game));
        assert_eq!(conf.bundle.exec, Some("TestStoreID.exe".to_string()));
        assert_eq!(conf.bundle.store_id, Some("TestStoreID".to_string()));
        assert_eq!(conf.bundle.background, true);
        assert_eq!(conf.bundle.prefer_xbox_mode, true);
        assert_eq!(conf.bundle.launcher, Some("wombat".to_string()));
        assert_eq!(
            conf.bundle.launcher_tags,
            vec!["Tag1", "Tag2", "Tag3", "Tag4"]
        );
        assert_eq!(
            conf.bundle.launcher_exec,
            Some("TestStoreID_Launcher.exe".to_string())
        );
    }

    #[test]
    fn test_deserialize_full_homebrew() {
        let input = r#"
[Bundle]
Name=Test Name With Spaces
Type=Application
Exec=TestHomebrewID.exe
HomebrewID=TestHomebrewID
Version=10213 124213 sfsd alpha
PreferXBoxMode=true
Launcher=wombat
"#;
        let conf: BundleConfig =
            serde_ini::from_str(input).expect("failed to deserialize test input");
        assert_eq!(conf.bundle.name, "Test Name With Spaces");
        assert!(matches!(conf.bundle.bundle_type, BundleType::Application));
        assert_eq!(conf.bundle.exec, Some("TestHomebrewID.exe".to_string()));
        assert_eq!(conf.bundle.homebrew_id, Some("TestHomebrewID".to_string()));
        assert_eq!(conf.bundle.background, false);
        assert_eq!(conf.bundle.prefer_xbox_mode, true);
        assert_eq!(conf.bundle.launcher, Some("wombat".to_string()));
        assert_eq!(conf.bundle.launcher_tags, Vec::<String>::new());
        assert_eq!(conf.bundle.launcher_exec, None);
    }

    #[test]
    fn test_deserialize_with_tags() {
        let input = r#"
[Bundle]
Name=TestName
Type=Game
StoreID=TestStoreID
Exec=TestStoreID.exe
LauncherTags=Tag1;Tag2;Tag3;Tag4
"#;
        let conf: BundleConfig =
            serde_ini::from_str(input).expect("failed to deserialize test input");
        assert_eq!(conf.bundle.name, "TestName");
        assert!(matches!(conf.bundle.bundle_type, BundleType::Game));
        assert_eq!(conf.bundle.store_id, Some("TestStoreID".to_string()));
        assert_eq!(conf.bundle.exec, Some("TestStoreID.exe".to_string()));
        assert_eq!(
            conf.bundle.launcher_tags,
            vec!["Tag1", "Tag2", "Tag3", "Tag4"]
        );
    }

    #[test]
    fn test_deserialize_no_tags() {
        let input = r#"
[Bundle]
Name=TestName
Type=Game
StoreID=TestStoreID
Exec=TestStoreID.exe
LauncherTags=
"#;
        let conf: BundleConfig =
            serde_ini::from_str(input).expect("failed to deserialize test input");
        assert_eq!(conf.bundle.name, "TestName");
        assert!(matches!(conf.bundle.bundle_type, BundleType::Game));
        assert_eq!(conf.bundle.store_id, Some("TestStoreID".to_string()));
        assert_eq!(conf.bundle.exec, Some("TestStoreID.exe".to_string()));
        assert_eq!(conf.bundle.launcher_tags, Vec::<String>::new());
    }

    #[test]
    fn test_deserialize_missing_tags() {
        let input = r#"
[Bundle]
Name=TestName
Type=Game
StoreID=TestStoreID
Exec=TestStoreID.exe
"#;
        let conf: BundleConfig =
            serde_ini::from_str(input).expect("failed to deserialize test input");
        assert_eq!(conf.bundle.name, "TestName");
        assert!(matches!(conf.bundle.bundle_type, BundleType::Game));
        assert_eq!(conf.bundle.store_id, Some("TestStoreID".to_string()));
        assert_eq!(conf.bundle.exec, Some("TestStoreID.exe".to_string()));
        assert_eq!(conf.bundle.launcher_tags, Vec::<String>::new());
    }

    #[test]
    fn test_deserialize_encrypted_image() {
        let input = r#"
[Bundle]
Name=Gamepad
Type=Application
StoreID=DummyStoreID
Version=5
EncryptedImage=bundle.img
"#;
        fn check(conf: &BundleConfig) {
            assert_eq!(conf.bundle.name, "Gamepad");
            assert!(matches!(conf.bundle.bundle_type, BundleType::Application));
            assert_eq!(conf.bundle.store_id, Some("DummyStoreID".to_string()));
            assert_eq!(conf.bundle.launcher_tags, Vec::<String>::new());
            assert_eq!(conf.bundle.encrypted_image, Some("bundle.img".to_string()));
        }
        let conf: BundleConfig =
            serde_ini::from_str(input).expect("failed to deserialize test input");
        check(&conf);

        let c = serde_ini::to_string(&conf).expect("Failed to serialize");
        let conf: BundleConfig =
            serde_ini::from_str(&c).expect("failed to re-deserialize test input");
        check(&conf);
    }
}
