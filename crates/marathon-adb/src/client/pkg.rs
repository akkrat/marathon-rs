//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/request/pkg/{StreamingPackageInstallRequest,
//! InstallRemotePackageRequest,UninstallRemotePackageRequest,InstallSplitPackageRequest}.kt` and
//! `.../pkg/multi/{InstallationPackage,CreateSessionResponse,CreateIndividualPackageSessionRequest,
//! WriteIndividualPackageRequest,InstallCommitRequest}.kt`.
//! Copyright (C) 2021 Anton Malinskiy; Apache-2.0.

use super::{AdbError, AndroidDebugBridgeClient, ComplexRequest, Feature, Request, Socket, Target, ValidationResponse};
use super::{
    abb::serialize_abb_exec,
    file_io::{AsyncFileReader, FileCompletion, io_error},
    request::create_base_request,
    shell::v1::{ShellCommandRequest, ShellCommandResult},
    sync::{directory::bash_escape, invalid},
    validation::extension,
};
use std::path::{Path, PathBuf};

pub struct InstallRemotePackageRequest {
    path: String,
    inner: ShellCommandRequest,
}
impl InstallRemotePackageRequest {
    pub fn new(path: impl Into<String>, reinstall: bool, extra_args: Vec<String>) -> Self {
        let path = path.into();
        let mut cmd = "pm install ".to_string();
        if reinstall {
            cmd.push_str("-r ");
        }
        if !extra_args.is_empty() {
            cmd.push_str(&extra_args.join(" "));
            cmd.push(' ');
        }
        cmd.push_str(&path);
        Self {
            path,
            inner: ShellCommandRequest::new(cmd),
        }
    }
}
impl Request for InstallRemotePackageRequest {
    fn serialize(&self) -> Vec<u8> {
        self.inner.serialize()
    }
    fn validate(&self) -> ValidationResponse {
        if self.path.ends_with(".apex") {
            invalid("APEX packages are only not compatible with InstallRemotePackageRequest".into())
        } else if !self.path.ends_with(".apk") {
            invalid(format!(
                "Unsupported package extension {}. Should be apk",
                self.path.rsplit('.').next().unwrap_or(&self.path)
            ))
        } else {
            ValidationResponse::SUCCESS
        }
    }
}
impl ComplexRequest for InstallRemotePackageRequest {
    type Output = ShellCommandResult;
    async fn read_element(&mut self, socket: &mut Socket) -> Result<Self::Output, AdbError> {
        self.inner.read_element(socket).await
    }
}

pub struct UninstallRemotePackageRequest(ShellCommandRequest);
impl UninstallRemotePackageRequest {
    pub fn new(package: &str, keep_data: bool) -> Self {
        Self(ShellCommandRequest::new(format!(
            "pm uninstall {}{package}",
            if keep_data { "-k " } else { "" }
        )))
    }
}
impl Request for UninstallRemotePackageRequest {
    fn serialize(&self) -> Vec<u8> {
        self.0.serialize()
    }
}
impl ComplexRequest for UninstallRemotePackageRequest {
    type Output = ShellCommandResult;
    async fn read_element(&mut self, s: &mut Socket) -> Result<Self::Output, AdbError> {
        self.0.read_element(s).await
    }
}

fn command(features: &[Feature], args: Vec<String>) -> Vec<u8> {
    if features.contains(&Feature::AbbExec) {
        serialize_abb_exec(&[vec!["package".into()], args].concat())
    } else {
        create_base_request(
            &Target::NonSpecified,
            &format!(
                "{} {}",
                if features.contains(&Feature::Cmd) {
                    "exec:cmd package"
                } else {
                    "exec:pm"
                },
                args.join(" ")
            ),
        )
    }
}
fn extra(features: &[Feature], args: &[String]) -> Vec<String> {
    args.iter()
        .map(|a| {
            if features.contains(&Feature::AbbExec) {
                a.clone()
            } else {
                bash_escape(a)
            }
        })
        .collect()
}

async fn validate_file(path: &Path, supported: &[&str], split: bool) -> Result<ValidationResponse, AdbError> {
    match tokio::fs::metadata(path).await {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(invalid(ValidationResponse::package_should_exist(path)));
        }
        Err(e) => return Err(io_error(e)),
        Ok(m) if !m.is_file() => return Ok(invalid(ValidationResponse::package_should_be_regular_file(path))),
        _ => (),
    }
    if split && extension(path) == "apex" {
        return Ok(invalid("Apex is not compatible with InstallSplitPackageRequest".into()));
    }
    if !supported.contains(&extension(path).as_str()) {
        return Ok(invalid(ValidationResponse::package_should_be_supported_extension(
            path, supported,
        )));
    }
    Ok(ValidationResponse::SUCCESS)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamingPackageInstallResult {
    pub output: String,
    pub success: bool,
}
pub struct StreamingPackageInstallRequest {
    pub pkg: PathBuf,
    pub supported_features: Vec<Feature>,
    pub reinstall: bool,
    pub extra_args: Vec<String>,
    validation: ValidationResponse,
    reader: Option<AsyncFileReader>,
    size: u64,
}
impl StreamingPackageInstallRequest {
    pub async fn prepare(
        pkg: PathBuf,
        features: Vec<Feature>,
        reinstall: bool,
        extra_args: Vec<String>,
    ) -> Result<Self, AdbError> {
        // File existence/type precede feature and extension checks in adam.
        let mut validation = match tokio::fs::metadata(&pkg).await {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                invalid(ValidationResponse::package_should_exist(&pkg))
            }
            Err(e) => return Err(io_error(e)),
            Ok(m) if !m.is_file() => invalid(ValidationResponse::package_should_be_regular_file(&pkg)),
            _ => ValidationResponse::SUCCESS,
        };
        if validation.success {
            validation = if !features.contains(&Feature::AbbExec) && !features.contains(&Feature::Cmd) {
                invalid(ValidationResponse::missing_either_feature(&[
                    Feature::AbbExec,
                    Feature::Cmd,
                ]))
            } else if extension(&pkg) == "apex" && !features.contains(&Feature::Apex) {
                invalid(ValidationResponse::missing_feature(Feature::Apex))
            } else {
                validate_file(&pkg, &["apk", "apex"], false).await?
            };
        }
        let reader = if validation.success {
            Some(AsyncFileReader::prepare(&pkg).await?)
        } else {
            None
        };
        let size = reader.as_ref().map(|r| r.size).unwrap_or(0);
        Ok(Self {
            pkg,
            supported_features: features,
            reinstall,
            extra_args,
            validation,
            reader,
            size,
        })
    }
    pub fn completion(&self) -> Option<FileCompletion> {
        self.reader.as_ref().map(AsyncFileReader::completion)
    }
}
impl Request for StreamingPackageInstallRequest {
    fn serialize(&self) -> Vec<u8> {
        let mut args = vec!["install".into()];
        args.extend(extra(&self.supported_features, &self.extra_args));
        if self.reinstall {
            args.push("-r".into());
        }
        args.extend(["-S".into(), self.size.to_string()]);
        if extension(&self.pkg) == "apex" {
            args.push("--apex".into());
        }
        command(&self.supported_features, args)
    }
    fn validate(&self) -> ValidationResponse {
        self.validation.clone()
    }
}
impl ComplexRequest for StreamingPackageInstallRequest {
    type Output = StreamingPackageInstallResult;
    async fn read_element(&mut self, s: &mut Socket) -> Result<Self::Output, AdbError> {
        let reader = self
            .reader
            .as_ref()
            .ok_or_else(|| AdbError::IllegalState("Unprepared package".into()))?;
        let result = s.write_file(reader).await;
        let closed = reader.close().await;
        result?;
        closed?;
        let output = s.read_status().await;
        Ok(StreamingPackageInstallResult {
            success: output.starts_with("Success"),
            output,
        })
    }
}

#[derive(Clone, Debug)]
pub enum InstallationPackage {
    SingleFile(PathBuf),
    ApkSplit(Vec<PathBuf>),
}
impl InstallationPackage {
    pub fn files(&self) -> &[PathBuf] {
        match self {
            Self::SingleFile(p) => std::slice::from_ref(p),
            Self::ApkSplit(p) => p,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateSessionResponse {
    pub id: String,
    pub output: String,
}
pub struct CreateIndividualPackageSessionRequest {
    pub pkg: InstallationPackage,
    pub pkg_list: Vec<InstallationPackage>,
    pub supported_features: Vec<Feature>,
    pub reinstall: bool,
    pub extra_args: Vec<String>,
}
impl Request for CreateIndividualPackageSessionRequest {
    fn serialize(&self) -> Vec<u8> {
        let mut args = vec!["install-create".into()];
        args.extend(extra(&self.supported_features, &self.extra_args));
        if self.reinstall {
            args.push("-r".into());
        }
        if self
            .pkg_list
            .iter()
            .flat_map(InstallationPackage::files)
            .any(|p| extension(p) == "apex")
        {
            args.push("--staged".into());
        }
        if self.pkg.files().iter().any(|p| extension(p) == "apex") {
            args.push("--apex".into());
        }
        command(&self.supported_features, args)
    }
}
impl ComplexRequest for CreateIndividualPackageSessionRequest {
    type Output = CreateSessionResponse;
    async fn read_element(&mut self, s: &mut Socket) -> Result<Self::Output, AdbError> {
        let output = s.read_status().await;
        if !output.contains("Success") {
            return Err(AdbError::RequestRejected(format!(
                "Failed to create multi-package session: {output}"
            )));
        }
        let id = output
            .split_once('[')
            .and_then(|(_, v)| v.split_once(']'))
            .map(|(id, _)| id)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| AdbError::RequestRejected("Failed to create multi-package session".into()))?
            .to_string();
        Ok(CreateSessionResponse { id, output })
    }
}

pub struct WriteIndividualPackageRequest {
    pub file: PathBuf,
    pub supported_features: Vec<Feature>,
    pub session: String,
    reader: AsyncFileReader,
}
impl WriteIndividualPackageRequest {
    pub async fn prepare(file: PathBuf, features: Vec<Feature>, session: String) -> Result<Self, AdbError> {
        let reader = AsyncFileReader::prepare(&file).await?;
        Ok(Self {
            file,
            supported_features: features,
            session,
            reader,
        })
    }
    pub fn completion(&self) -> FileCompletion {
        self.reader.completion()
    }
}
impl Request for WriteIndividualPackageRequest {
    fn serialize(&self) -> Vec<u8> {
        command(
            &self.supported_features,
            vec![
                "install-write".into(),
                "-S".into(),
                self.reader.size.to_string(),
                self.session.clone(),
                self.file
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                "-".into(),
            ],
        )
    }
}
impl ComplexRequest for WriteIndividualPackageRequest {
    type Output = String;
    async fn read_element(&mut self, s: &mut Socket) -> Result<String, AdbError> {
        let result = s.write_file(&self.reader).await;
        let closed = self.reader.close().await;
        result?;
        closed?;
        let output = s.read_status().await;
        if !output.contains("Success") {
            return Err(AdbError::RequestRejected(format!(
                "Failed to write package {}: {output}",
                self.file.display()
            )));
        }
        Ok(output)
    }
}

pub struct InstallCommitRequest {
    pub parent_session: String,
    pub supported_features: Vec<Feature>,
    pub abandon: bool,
}
impl InstallCommitRequest {
    pub fn new(session: impl Into<String>, features: Vec<Feature>, abandon: bool) -> Self {
        Self {
            parent_session: session.into(),
            supported_features: features,
            abandon,
        }
    }
}
impl Request for InstallCommitRequest {
    fn serialize(&self) -> Vec<u8> {
        command(
            &self.supported_features,
            vec![
                if self.abandon {
                    "install-abandon"
                } else {
                    "install-commit"
                }
                .into(),
                self.parent_session.clone(),
            ],
        )
    }
}
impl ComplexRequest for InstallCommitRequest {
    type Output = String;
    async fn read_element(&mut self, s: &mut Socket) -> Result<String, AdbError> {
        let output = s.read_status().await;
        if output.contains("Failure") {
            return Err(AdbError::RequestRejected(format!(
                "Failed to finalize session {}: {output}",
                self.parent_session
            )));
        }
        Ok(output)
    }
}

pub struct InstallSplitPackageRequest {
    files: Vec<PathBuf>,
    features: Vec<Feature>,
    reinstall: bool,
    extra_args: Vec<String>,
    total_size: u64,
    validation: ValidationResponse,
    completion: super::file_io::FileCompletions,
}
impl InstallSplitPackageRequest {
    pub async fn prepare(
        files: Vec<PathBuf>,
        features: Vec<Feature>,
        reinstall: bool,
        extra_args: Vec<String>,
    ) -> Result<Self, AdbError> {
        let mut validation = ValidationResponse::SUCCESS;
        let mut total_size = 0u64;
        for file in &files {
            validation = validate_file(file, &["apk", "dm", "fsv_sig"], true).await?;
            if !validation.success {
                break;
            }
            total_size = total_size.wrapping_add(tokio::fs::metadata(file).await.map_err(io_error)?.len());
        }
        if validation.success && !files.iter().any(|p| extension(p) == "apk") {
            validation = invalid(ValidationResponse::one_of_files_should_be("apk"));
        }
        Ok(Self {
            files,
            features,
            reinstall,
            extra_args,
            total_size,
            validation,
            completion: Default::default(),
        })
    }
    pub fn completion(&self) -> super::file_io::FileCompletions {
        self.completion.clone()
    }
    pub fn validate(&self) -> ValidationResponse {
        self.validation.clone()
    }
    pub async fn execute(self, client: &AndroidDebugBridgeClient, serial: Option<&str>) -> Result<String, AdbError> {
        if !self.validation.success {
            return Err(AdbError::RequestValidation(format!(
                "Request InstallSplitPackageRequest did not pass validation: {}",
                self.validation.message.unwrap_or_default()
            )));
        }
        let pkg = InstallationPackage::ApkSplit(self.files.clone());
        let mut extra_args = self.extra_args;
        extra_args.push(format!("-S{}", self.total_size as i64));
        let session = client
            .execute(
                CreateIndividualPackageSessionRequest {
                    pkg: pkg.clone(),
                    pkg_list: vec![pkg],
                    supported_features: self.features.clone(),
                    reinstall: self.reinstall,
                    extra_args,
                },
                serial,
            )
            .await?;
        let mut response = format!("{}\n", session.output);
        let result: Result<String, AdbError> = async {
            for file in self.files {
                let request =
                    WriteIndividualPackageRequest::prepare(file, self.features.clone(), session.id.clone()).await?;
                let completion = request.completion();
                self.completion.register(completion.clone()).await;
                let output = client.execute(request, serial).await;
                completion.close().await?;
                response.push_str(&output?);
                response.push('\n');
            }
            response.push_str(
                &client
                    .execute(
                        InstallCommitRequest::new(&session.id, self.features.clone(), false),
                        serial,
                    )
                    .await?,
            );
            response.push('\n');
            Ok(response)
        }
        .await;
        if let Err(e) = &result
            && e.is_exception()
        {
            let abandon = client
                .execute(InstallCommitRequest::new(session.id, self.features, true), serial)
                .await;
            if let Err(e) = abandon
                && !e.is_exception()
            {
                return Err(e);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests;
