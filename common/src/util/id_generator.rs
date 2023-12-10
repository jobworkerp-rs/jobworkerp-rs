use anyhow::{anyhow, Result};
use rand::Rng;
use snowflake::SnowflakeIdBucket;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct MockIdGenerator {
    id: i64,
}
impl Default for MockIdGenerator {
    fn default() -> Self {
        Self::new()
    }
}
impl MockIdGenerator {
    pub fn new() -> Self {
        Self { id: 1 }
    }
    pub fn generate_id(&self) -> i64 {
        self.id
    }
    pub fn set_id(&mut self, id: i64) {
        self.id = id;
    }
}

#[derive(Clone)]
pub enum IDGenerator {
    Snowflake(Arc<Mutex<SnowflakeIdBucket>>),
    Mock(MockIdGenerator),
}

impl IDGenerator {
    pub fn generate(&mut self) -> Result<i64> {
        match self {
            // generete and pool ids (max 4092 id) : https://github.com/BinChengZhao/snowflake-rs/blob/master/src/lib.rs#L252
            IDGenerator::Snowflake(gen) => gen
                .lock()
                .map(|mut g| g.get_id())
                .map_err(|e| anyhow!(format!("generate id error: {:?}", e))),
            IDGenerator::Mock(gen) => Ok(gen.generate_id()),
        }
    }
}

// bit: smaller than 32
fn random_node(bit: u32) -> u32 {
    let mut rng = rand::thread_rng();
    let n1: u32 = rng.gen();
    let n = ((1 << bit) - 1) & n1;
    tracing::warn!("using random node num for id generator: {}", n);
    n
}

// default 10bit node
pub fn new_generator_by_ip() -> IDGenerator {
    let node = iputil::resolve_host_node(10).unwrap_or_else(|| random_node(10));
    tracing::debug!("using node num for id generator: {}", node);
    // XXX machine_id <= 5bit
    // ref. https://github.com/BinChengZhao/snowflake-rs/blob/16dec24a484852ea0706de95966dfc791e3cdcbf/src/lib.rs#L171
    let gen = SnowflakeIdBucket::new((node >> 5) as i32, node as i32);
    IDGenerator::Snowflake(Arc::new(Mutex::new(gen)))
}

// node_id: only lower 10bit is valid
pub fn new_generator(node_id: i32) -> IDGenerator {
    let gen = SnowflakeIdBucket::new(node_id >> 5, node_id);
    IDGenerator::Snowflake(Arc::new(Mutex::new(gen)))
}

#[tokio::test]
async fn thread_safe_test() {
    use itertools::Itertools;
    use std::collections::HashSet;
    use tokio::task::JoinSet;

    let mut set = JoinSet::new();
    let gen: Arc<Mutex<IDGenerator>> = Arc::new(Mutex::new(new_generator_by_ip()));

    let _jh = (0..1000)
        .map(|_j| {
            let gen2 = gen.clone();
            set.spawn(async move {
                (0..1000)
                    .map(|_i| gen2.lock().unwrap().generate())
                    .collect_vec()
            })
        })
        .collect_vec();

    let mut hash = HashSet::<i64>::new();
    while let Some(res) = set.join_next().await {
        let ids: HashSet<i64> = res
            .unwrap()
            .iter()
            .map(|v| *v.iter().cloned().collect_vec().first().unwrap())
            .collect();
        hash.extend(&ids);
    }
    assert_eq!(hash.len(), 1000 * 1000);
}

pub mod iputil {
    use once_cell::sync::Lazy;

    use pnet::{
        datalink,
        ipnetwork::{IpNetwork, Ipv4Network},
    };
    use std::cmp;

    pub static IP_LOCAL: Lazy<Ipv4Network> = Lazy::new(|| "127.0.0.0/8".parse().unwrap());
    static IP_CLASS_A: Lazy<Ipv4Network> = Lazy::new(|| "10.0.0.0/8".parse().unwrap());
    static IP_CLASS_B: Lazy<Ipv4Network> = Lazy::new(|| "172.16.0.0/12".parse().unwrap());
    static IP_CLASS_C: Lazy<Ipv4Network> = Lazy::new(|| "192.168.0.0/16".parse().unwrap());

    #[inline]
    /// `valid_bit`: max bit number for node
    pub fn resolve_host_node(valid_bit: u32) -> Option<u32> {
        let address = resolve_host_ipv4();
        address.map(|a| host_node(a, valid_bit))
    }

    pub fn resolve_host_ipv4() -> Option<Ipv4Network> {
        let mut address: Option<Ipv4Network> = None;
        let mut p = 0;
        for iface in datalink::interfaces() {
            for ip in iface.ips {
                match ip {
                    IpNetwork::V4(v4) => {
                        if p < priority(v4) {
                            address = Some(v4);
                            p = priority(v4);
                        }
                    }
                    IpNetwork::V6(v6) => {
                        tracing::debug!(
                            "ipv6 address {:?} not supported for snowflake node num (use random)",
                            v6
                        );
                    }
                }
            }
        }
        tracing::debug!("priority: {}, ip: {:?}", p, address);
        address
    }

    #[inline]
    pub fn host_node(ip: Ipv4Network, valid_bit: u32) -> u32 {
        let host_mask = cmp::min(
            (0xffff_ffff_u64 >> ip.prefix()) as u32,
            !(0xffff_ffff_u64 << valid_bit) as u32,
        );
        u32::from(ip.ip()) & host_mask
    }
    fn priority(ip: Ipv4Network) -> usize {
        for (i, net) in [*IP_LOCAL, *IP_CLASS_A, *IP_CLASS_B, *IP_CLASS_C]
            .iter()
            .enumerate()
        {
            if net.contains(ip.ip()) {
                return i;
            }
        }
        // global?
        4
    }
    #[test]
    fn host_num_test() {
        assert_eq!(host_node("192.168.254.254/24".parse().unwrap(), 8), 254);
        assert_eq!(host_node("192.168.254.254/24".parse().unwrap(), 16), 254);
        // lower 10bit
        assert_eq!(host_node("192.168.254.254/16".parse().unwrap(), 10), 0x2fe);
        assert_eq!(host_node("192.168.254.254/16".parse().unwrap(), 32), 0xfefe);
    }
    #[test]
    fn priority_test() {
        assert_eq!(priority("127.168.254.254/8".parse().unwrap()), 0);
        assert_eq!(priority("10.168.254.254/14".parse().unwrap()), 1);
        assert_eq!(priority("172.16.254.254/12".parse().unwrap()), 2);
        assert_eq!(priority("192.168.254.254/24".parse().unwrap()), 3);
        assert_eq!(priority("12.168.254.254/24".parse().unwrap()), 4);
    }
}
