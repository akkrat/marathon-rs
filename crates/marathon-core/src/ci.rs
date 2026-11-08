//! Port of core/src/main/kotlin/com/malinskiy/marathon/integrations/ci/CI.kt
//! and CIIntegrationFactory.kt, None.kt, Teamcity.kt.

use marathon_config::schema::CiConfiguration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ci {
    None,
    Teamcity,
}
impl Ci {
    pub fn from_config(configuration: CiConfiguration, environment: &[(String, String)]) -> Self {
        match configuration {
            CiConfiguration::None => Self::None,
            CiConfiguration::Teamcity => Self::Teamcity,
            // C3: Kotlin checks values, not the TEAMCITY_VERSION key.
            CiConfiguration::Auto if environment.iter().any(|(_, v)| *v == String::from("TEAMCITY_VERSION")) => {
                Self::Teamcity
            }
            CiConfiguration::Auto => Self::None,
        }
    }
    pub fn build_progress(self, progress: i32) -> Option<String> {
        match self {
            Self::None => None,
            Self::Teamcity => Some(format!("##teamcity[buildStatus text='Marathon run: {progress} %']\n")),
        }
    }
}
