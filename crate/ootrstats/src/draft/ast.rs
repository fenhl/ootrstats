use {
    syn::{
        *,
        parse::{
            Parse,
            ParseStream,
        },
        punctuated::Punctuated,
    },
    super::*,
};

impl Parse for Spec {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let mut groups = None;
        let mut steps = None;
        let mut settings = None;
        while !input.is_empty() {
            let field_name = input.parse::<Ident>()?;
            input.parse::<Token![:]>()?;
            match &*field_name.to_string() {
                "groups" => {
                    let mut new_groups = BTreeMap::default();
                    let content;
                    braced!(content in input);
                    for Group { name, settings } in content.parse_terminated(Group::parse, Token![,])? {
                        if new_groups.insert(name.clone(), settings).is_some() {
                            return Err(input.error(format!("draft spec defines multiple groups named {name:?}")))
                        }
                    }
                    if groups.replace(new_groups).is_some() {
                        return Err(input.error("groups specified multiple times"))
                    }
                }
                "steps" => {
                    let content;
                    bracketed!(content in input);
                    let new_steps = content.parse_terminated(Step::parse, Token![,])?
                        .into_iter()
                        .map(|Step { team, kind }| (team, kind))
                        .collect();
                    if steps.replace(new_steps).is_some() {
                        return Err(input.error("steps specified multiple times"))
                    }
                }
                "settings" => if settings.replace(input.parse()?).is_some() {
                    return Err(input.error("settings specified multiple times"))
                },
                field_name => return Err(input.error(format!("unexpected draft spec field: {field_name}"))),
            }
        }
        Ok(Self {
            groups: groups.ok_or_else(|| input.error("missing groups field in draft spec"))?,
            steps: steps.ok_or_else(|| input.error("missing steps field in draft spec"))?,
            settings: settings.ok_or_else(|| input.error("missing settings field in draft spec"))?,
        })
    }
}

struct Group {
    name: String,
    settings: BTreeMap<String, Setting>,
}

impl Parse for Group {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let name = input.parse::<LitStr>()?.value();
        input.parse::<Token![:]>()?;
        let mut settings = BTreeMap::default();
        let content;
        braced!(content in input);
        for ParseSetting { name, default, other } in content.parse_terminated(ParseSetting::parse, Token![,])? {
            if settings.insert(name.clone(), Setting { default, other }).is_some() {
                return Err(input.error(format!("draft group defines multiple settings named {name}")))
            }
        }
        Ok(Self { name, settings })
    }
}

struct ParseSetting {
    name: String,
    default: String,
    other: BTreeSet<String>,
}

impl Parse for ParseSetting {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let name = input.parse::<Ident>()?.to_string();
        input.parse::<Token![:]>()?;
        let mut default = None;
        let mut other = BTreeSet::default();
        let content;
        braced!(content in input);
        for option in content.parse_terminated(ParseOption::parse, Token![,])? {
            match option {
                ParseOption::Default(new_default) => if default.replace(new_default).is_some() {
                    return Err(input.error("default specified multiple times"))
                },
                ParseOption::Other(name) => if !other.insert(name.clone()) {
                    return Err(input.error(format!("draft setting defines multiple options named {name}")))
                },
            }
        }
        Ok(Self {
            default: default.ok_or_else(|| input.error("missing default option in draft setting"))?,
            name, other,
        })
    }
}

enum ParseOption {
    Default(String),
    Other(String),
}

impl Parse for ParseOption {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let lookahead = input.lookahead1();
        Ok(if lookahead.peek(Ident) {
            if input.parse::<Ident>()?.to_string() != "default" {
                return Err(input.error("unexpected identifier in parse option"))
            }
            input.parse::<Token![:]>()?;
            Self::Default(input.parse::<LitStr>()?.value())
        } else if lookahead.peek(LitStr) {
            Self::Other(input.parse::<LitStr>()?.value())
        } else {
            return Err(lookahead.error())
        })
    }
}

struct Step {
    team: Team,
    kind: StepKind,
}

impl Parse for Step {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let team = input.parse()?;
        input.parse::<Token![:]>()?;
        let kind = input.parse()?;
        Ok(Self { team, kind })
    }
}

impl Parse for Team {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let name = input.parse::<Ident>()?;
        Ok(match &*name.to_string() {
            "A" => Self::A,
            "B" => Self::B,
            name => return Err(input.error(format!("unexpected team name: {name}"))),
        })
    }
}

impl Parse for StepKind {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let name = input.parse::<Ident>()?;
        Ok(match &*name.to_string() {
            "Ban" => {
                let mut skippable = None;
                let content;
                braced!(content in input);
                for config in content.parse_terminated(BanConfig::parse, Token![,])? {
                    match config {
                        BanConfig::Skippable(new_skippable) => if skippable.replace(new_skippable).is_some() {
                            return Err(input.error("skippable specified multiple times"))
                        },
                    }
                }
                Self::Ban {
                    skippable: skippable.ok_or_else(|| input.error("missing skippable value in ban step"))?,
                }
            }
            "Pick" => {
                let mut skippable = None;
                let mut defaultable = None;
                let content;
                braced!(content in input);
                for config in content.parse_terminated(PickConfig::parse, Token![,])? {
                    match config {
                        PickConfig::Skippable(new_skippable) => if skippable.replace(new_skippable).is_some() {
                            return Err(input.error("skippable specified multiple times"))
                        },
                        PickConfig::Defaultable(new_defaultable) => if defaultable.replace(new_defaultable).is_some() {
                            return Err(input.error("defaultable specified multiple times"))
                        },
                    }
                }
                Self::Pick {
                    skippable: skippable.ok_or_else(|| input.error("missing skippable value in pick step"))?,
                    defaultable: defaultable.ok_or_else(|| input.error("missing defaultable value in pick step"))?,
                }
            }
            name => return Err(input.error(format!("unexpected step kind: {name}"))),
        })
    }
}

enum BanConfig {
    Skippable(bool),
}

impl Parse for BanConfig {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let field_name = input.parse::<Ident>()?;
        input.parse::<Token![:]>()?;
        Ok(match &*field_name.to_string() {
            "skippable" => Self::Skippable(input.parse::<LitBool>()?.value),
            field_name => return Err(input.error(format!("unexpected ban step config field: {field_name}"))),
        })
    }
}

enum PickConfig {
    Skippable(bool),
    Defaultable(Defaultable),
}

impl Parse for PickConfig {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let field_name = input.parse::<Ident>()?;
        input.parse::<Token![:]>()?;
        Ok(match &*field_name.to_string() {
            "skippable" => Self::Skippable(input.parse::<LitBool>()?.value),
            "defaultable" => Self::Defaultable(input.parse()?),
            field_name => return Err(input.error(format!("unexpected ban step config field: {field_name}"))),
        })
    }
}

impl Parse for Defaultable {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let lookahead = input.lookahead1();
        Ok(if lookahead.peek(Ident) {
            if input.parse::<Ident>()?.to_string() != "has_picked" {
                return Err(input.error("unexpected identifier in defaultable value"))
            }
            Self::HasPicked
        } else if lookahead.peek(LitBool) {
            if input.parse::<LitBool>()?.value {
                Self::True
            } else {
                Self::False
            }
        } else {
            return Err(lookahead.error())
        })
    }
}

impl Parse for Settings {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let lookahead = input.lookahead1();
        Ok(if lookahead.peek(Token![match]) {
            input.parse::<Token![match]>()?;
            let mut fallback = None;
            let setting = input.parse::<Ident>()?.to_string();
            let mut arms = BTreeMap::default();
            let content;
            braced!(content in input);
            for SettingsMatchArm { options, value } in content.parse_terminated(SettingsMatchArm::parse, Token![,])? {
                if fallback.is_some() {
                    return Err(input.error("wildcard arm must be the last match arm"))
                }
                if options.is_empty() {
                    if fallback.replace(Box::new(value)).is_some() {
                        return Err(input.error("multiple wildcard arms in draft settings"))
                    }
                } else {
                    for option in options {
                        if arms.insert(option.clone(), value.clone()).is_some() {
                            return Err(input.error(format!("match arm in draft settings matches on {option:?} multiple times")))
                        }
                    }
                }
            }
            Self::Match { setting, arms, fallback }
        } else if lookahead.peek(Ident) {
            Self::Setting(input.parse::<Ident>()?.to_string())
        } else if lookahead.peek(LitBool) {
            Self::Bool(input.parse::<LitBool>()?.value)
        } else if lookahead.peek(LitInt) {
            let lit = input.parse::<LitInt>()?;
            Self::Number(lit.base10_parse::<u64>()?.into())
        } else if lookahead.peek(LitStr) {
            Self::String(input.parse::<LitStr>()?.value())
        } else if lookahead.peek(token::Brace) {
            let mut obj = BTreeMap::default();
            let content;
            braced!(content in input);
            for SettingsEntry { name, value } in content.parse_terminated(SettingsEntry::parse, Token![,])? {
                if obj.insert(name.clone(), value).is_some() {
                    return Err(input.error(format!("draft settings define multiple entries named {name:?}")))
                }
            }
            Self::Object(obj)
        } else if lookahead.peek(token::Bracket) {
            let content;
            bracketed!(content in input);
            Self::Array(content.parse_terminated(Settings::parse, Token![,])?.into_iter().collect())
        } else {
            return Err(lookahead.error())
        })
    }
}

struct SettingsEntry {
    name: String,
    value: Settings,
}

impl Parse for SettingsEntry {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let name = input.parse::<LitStr>()?.value();
        input.parse::<Token![:]>()?;
        let value = input.parse()?;
        Ok(Self { name, value })
    }
}

struct SettingsMatchArm {
    options: Vec<String>,
    value: Settings,
}

impl Parse for SettingsMatchArm {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let lookahead = input.lookahead1();
        let options = if lookahead.peek(Token![_]) {
            input.parse::<Token![_]>()?;
            Vec::default()
        } else if lookahead.peek(LitStr) {
            Punctuated::<LitStr, Token![|]>::parse_separated_nonempty(input)?
                .into_iter()
                .map(|option| option.value())
                .collect()
        } else {
            return Err(lookahead.error())
        };
        input.parse::<Token![=>]>()?;
        let value = input.parse()?;
        Ok(Self { options, value })
    }
}
