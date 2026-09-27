use std::{collections::HashSet, net::IpAddr};

use crate::jobworkerp::runner::{
    SandboxNetworkConfig, SandboxNetworkRule, sandbox_network_destination::Destination,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use microsandbox::{NetworkAction, NetworkPolicy, NetworkProfile, NetworkRule};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct ValidatedNetworkConfig {
    pub policy: NetworkPolicy,
    pub max_tcp_connections: usize,
    pub max_udp_connections: usize,
}

pub fn build_network_policy(config: &SandboxNetworkConfig) -> Result<NetworkPolicy> {
    Ok(validate_network_config(config)?
        .map(|network| network.policy)
        .unwrap_or_else(NetworkPolicy::none))
}

pub(super) fn validate_network_config(
    config: &SandboxNetworkConfig,
) -> Result<Option<ValidatedNetworkConfig>> {
    let enabled = config.enabled.unwrap_or(false);
    if !enabled {
        ensure!(
            config.profiles.is_empty()
                && config.rules.is_empty()
                && config.max_tcp_connections.is_none()
                && config.max_udp_connections.is_none(),
            "network options cannot be set while networking is disabled"
        );
        return Ok(None);
    }

    let tcp = config
        .max_tcp_connections
        .filter(|limit| *limit > 0)
        .ok_or_else(|| anyhow!("enabled network requires a positive finite max_tcp_connections"))?;
    let udp = config
        .max_udp_connections
        .filter(|limit| *limit > 0)
        .ok_or_else(|| anyhow!("enabled network requires a positive finite max_udp_connections"))?;

    let mut profiles = Vec::with_capacity(config.profiles.len());
    for profile in &config.profiles {
        profiles.push(match profile.as_str() {
            "public" => NetworkProfile::Public,
            "private" => NetworkProfile::Private,
            "host" => NetworkProfile::Host,
            _ => bail!("unsupported network profile: {profile}"),
        });
    }

    let metadata_deny = parse_sdk_rule(json!({
        "direction": "egress",
        "destination": {"group": "metadata"},
        "protocols": [],
        "ports": [],
        "action": "deny"
    }))?;
    let mut policy = NetworkPolicy::from_profiles(profiles);
    policy.default_egress = NetworkAction::Deny;
    policy.default_ingress = NetworkAction::Deny;
    policy.rules.insert(0, metadata_deny);

    let mut explicit_rules = Vec::with_capacity(config.rules.len());
    for rule in &config.rules {
        explicit_rules.push(parse_worker_rule(rule)?);
    }
    policy.rules.splice(1..1, explicit_rules);

    Ok(Some(ValidatedNetworkConfig {
        policy,
        max_tcp_connections: tcp as usize,
        max_udp_connections: udp as usize,
    }))
}

fn parse_worker_rule(rule: &SandboxNetworkRule) -> Result<NetworkRule> {
    let action = match rule.action.as_str() {
        "allow" => "allow",
        "deny" => "deny",
        _ => bail!("network rule action must be allow or deny"),
    };
    let destination = rule
        .destination
        .as_ref()
        .and_then(|destination| destination.destination.as_ref())
        .ok_or_else(|| anyhow!("network rule requires exactly one destination"))?;
    let destination = match destination {
        Destination::Group(group) => {
            ensure!(
                matches!(group.as_str(), "public" | "private" | "host"),
                "unsupported network destination group: {group}"
            );
            json!({"group": group})
        }
        Destination::Ip(ip) => {
            let parsed: IpAddr = ip
                .parse()
                .context("network rule ip must be a valid IP address")?;
            let cidr = match parsed {
                IpAddr::V4(_) => format!("{parsed}/32"),
                IpAddr::V6(_) => format!("{parsed}/128"),
            };
            json!({"cidr": cidr})
        }
        Destination::Cidr(cidr) => {
            validate_cidr(cidr)?;
            json!({"cidr": cidr})
        }
        Destination::Domain(domain) => {
            validate_domain(domain)?;
            json!({"domain": domain})
        }
        Destination::DomainSuffix(domain_suffix) => {
            validate_domain(domain_suffix)?;
            json!({"domain_suffix": domain_suffix})
        }
        Destination::Any(true) => json!("any"),
        Destination::Any(false) => bail!("network destination any must be true"),
    };

    let protocols = if rule.protocols.is_empty() {
        vec!["tcp", "udp"]
    } else {
        rule.protocols
            .iter()
            .map(|protocol| match protocol.as_str() {
                "tcp" => Ok("tcp"),
                "udp" => Ok("udp"),
                _ => bail!("network rule protocols may contain only tcp and udp"),
            })
            .collect::<Result<Vec<_>>>()?
    };
    let mut seen_protocols = HashSet::new();
    ensure!(
        protocols
            .iter()
            .all(|protocol| seen_protocols.insert(*protocol)),
        "network rule protocols cannot contain duplicates"
    );

    let mut ports = Vec::with_capacity(rule.ports.len());
    for port in &rule.ports {
        ensure!(
            (1..=u16::MAX as u32).contains(&port.start)
                && (1..=u16::MAX as u32).contains(&port.end)
                && port.start <= port.end,
            "network port ranges must be between 1 and 65535 with start <= end"
        );
        ports.push(json!({"start": port.start, "end": port.end}));
    }

    parse_sdk_rule(json!({
        "direction": "egress",
        "destination": destination,
        "protocols": protocols,
        "ports": ports,
        "action": action
    }))
}

fn parse_sdk_rule(value: Value) -> Result<NetworkRule> {
    serde_json::from_value(value).context("failed to build microsandbox network rule")
}

fn validate_cidr(cidr: &str) -> Result<()> {
    let (ip, prefix) = cidr
        .split_once('/')
        .ok_or_else(|| anyhow!("network cidr must include a prefix"))?;
    let ip: IpAddr = ip
        .parse()
        .context("network cidr must use a valid IP address")?;
    let prefix: u8 = prefix.parse().context("network cidr prefix is invalid")?;
    ensure!(
        prefix <= if ip.is_ipv4() { 32 } else { 128 },
        "network cidr prefix is out of range"
    );
    Ok(())
}

fn validate_domain(domain: &str) -> Result<()> {
    ensure!(!domain.is_empty(), "network domain must not be empty");
    ensure!(
        domain.is_ascii()
            && !domain.chars().any(char::is_whitespace)
            && !domain.contains('/')
            && !domain.contains('\0'),
        "network domain must be an ASCII host name"
    );
    Ok(())
}

pub(super) fn validate_network_disabled_override(config: &SandboxNetworkConfig) -> Result<()> {
    ensure!(
        config.enabled == Some(false),
        "job network override may only explicitly disable networking"
    );
    ensure!(
        config.profiles.is_empty()
            && config.rules.is_empty()
            && config.max_tcp_connections.is_none()
            && config.max_udp_connections.is_none(),
        "job network disable override cannot add rules, profiles, or limits"
    );
    Ok(())
}
