use {
    std::{
        borrow::Cow,
        collections::{
            HashMap,
            HashSet,
        },
    },
    async_proto::Protocol,
    rand::{
        prelude::*,
        rng,
    },
    serde_json::Value as Json,
};

mod ast;

#[derive(Clone, Protocol)]
struct Setting {
    default: String,
    other: HashSet<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Protocol)]
enum Team {
    A,
    B,
}

#[derive(Clone, Copy, Protocol)]
enum Defaultable {
    False,
    True,
    HasPicked,
}

#[derive(Clone, Copy, Protocol)]
enum StepKind {
    Ban {
        skippable: bool,
    },
    Pick {
        skippable: bool,
        defaultable: Defaultable,
    },
}

#[derive(Clone, Protocol)]
enum Settings {
    Bool(bool),
    Number(serde_json::Number),
    String(String),
    Array(Vec<Settings>),
    Object(HashMap<String, Settings>),
    Setting(String),
    Match {
        setting: String,
        arms: HashMap<String, Settings>,
        fallback: Option<Box<Settings>>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("match draft setting {setting} missing arm for option {option:?}")]
    MissingOption {
        setting: String,
        option: String,
    },
    #[error("settings should be a JSON object, got {0}")]
    NonObjectSettings(Json),
    #[error("tried to match on unknown option {option:?} of draft setting {setting}")]
    UnknownOption {
        setting: String,
        option: String,
    },
    #[error("tried to match on unknown draft setting {0}")]
    UnknownSetting(String),
}

impl Settings {
    fn resolve(&self, groups: &HashMap<String, HashMap<String, Setting>>, picks: &HashMap<&str, &str>) -> Result<Json, ResolveError> {
        Ok(match self {
            Self::Bool(b) => Json::Bool(*b),
            Self::Number(n) => Json::Number(n.clone()),
            Self::String(s) => Json::String(s.clone()),
            Self::Array(arr) => arr.iter()
                .map(|value| Ok::<_, ResolveError>(value.resolve(groups, picks)?))
                .collect::<Result<_, _>>()?,
            Self::Object(obj) => obj.iter()
                .map(|(key, value)| Ok::<_, ResolveError>((key.clone(), value.resolve(groups, picks)?)))
                .collect::<Result<_, _>>()?,
            Self::Setting(setting) => {
                let all_options = groups.values()
                    .find_map(|group| group.get(setting))
                    .ok_or_else(|| ResolveError::UnknownSetting(setting.clone()))?;
                Json::String(picks.get(&**setting).copied().unwrap_or(&all_options.default).to_owned())
            }
            Self::Match { setting, arms, fallback } => {
                let all_options = groups.values()
                    .find_map(|group| group.get(setting))
                    .ok_or_else(|| ResolveError::UnknownSetting(setting.clone()))?;
                if fallback.is_none() {
                    if !arms.contains_key(&all_options.default) {
                        return Err(ResolveError::MissingOption {
                            setting: setting.clone(),
                            option: all_options.default.clone(),
                        })
                    }
                    for option in &all_options.other {
                        if !arms.contains_key(option) {
                            return Err(ResolveError::MissingOption {
                                setting: setting.clone(),
                                option: option.clone(),
                            })
                        }
                    }
                }
                for option in arms.keys() {
                    if *option != all_options.default && !all_options.other.contains(option) {
                        return Err(ResolveError::UnknownOption {
                            setting: setting.clone(),
                            option: option.clone(),
                        })
                    }
                }
                arms.get(picks.get(&**setting).copied().unwrap_or(&all_options.default))
                    .or(fallback.as_deref())
                    .expect("checked above")
                    .resolve(groups, picks)?
            }
        })
    }
}

#[derive(Clone, Protocol)]
pub struct Spec {
    groups: HashMap<String, HashMap<String, Setting>>,
    steps: Vec<(Team, StepKind)>,
    settings: Settings,
}

impl Spec {
    pub(crate) fn complete_randomly(&self) -> Result<HashMap<Cow<'static, str>, Json>, ResolveError> {
        let Self { groups, steps, settings } = self;
        let mut rng = rng();
        let mut has_picked = HashSet::new();
        let mut picked_settings = HashMap::<&str, &str>::default();
        for (team, step) in steps {
            match step {
                StepKind::Ban { skippable } => {
                    let choice = groups.values().flatten()
                        .filter(|&(setting_name, _)| !picked_settings.contains_key(&**setting_name))
                        .map(Some)
                        .chain(skippable.then_some(None))
                        .choose(&mut rng)
                        .unwrap();
                    if let Some((setting_name, setting)) = choice {
                        picked_settings.insert(setting_name, &setting.default);
                    }
                }
                StepKind::Pick { skippable, defaultable } => {
                    let choice = groups.values().flatten()
                        .filter(|&(setting_name, _)| !picked_settings.contains_key(&**setting_name))
                        .flat_map(|(setting_name, options)|
                            options.other.iter().map(move |option| (setting_name, option, false))
                                .chain(match defaultable {
                                    Defaultable::False => false,
                                    Defaultable::True => true,
                                    Defaultable::HasPicked => has_picked.contains(&team),
                                }.then(|| (setting_name, &options.default, true)))
                        )
                        .map(Some)
                        .chain(skippable.then_some(None))
                        .choose(&mut rng)
                        .unwrap();
                    if let Some((setting_name, setting, is_default)) = choice {
                        picked_settings.insert(setting_name, setting);
                        if !is_default {
                            has_picked.insert(team);
                        }
                    }
                }
            }
        }
        match settings.resolve(&groups, &picked_settings)? {
            Json::Object(settings) => Ok(settings.into_iter().map(|(key, value)| (Cow::Owned(key), value)).collect()),
            value => Err(ResolveError::NonObjectSettings(value)),
        }
    }
}
