// TODO break this into files

use chrono::{DateTime, Utc};
use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment, Profile,
};
use lazy_static::lazy_static;
use rand::{distributions::Alphanumeric, Rng};
use rocket::serde::{
    json::{from_str, Json},
    Deserialize, Serialize,
};
use rocket::{fairing::AdHoc, State};
use sha2::{Digest, Sha256};
use std::convert::TryInto;
use std::fmt::Debug;
use std::net::{IpAddr, Ipv4Addr};
use std::str;
use std::str::FromStr;
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::thread;
use url::Url;

#[macro_use]
extern crate rocket;
extern crate lazy_static;

#[launch]
fn rocket() -> _ {
    let figment = Figment::from(rocket::Config::default())
        .merge(Serialized::defaults(Config::default()))
        .merge(Toml::file("Rocket.toml").nested())
        .merge(Env::prefixed("APP_").global())
        .select(Profile::from_env_or("APP_PROFILE", "default"));

    #[derive(Debug, Deserialize, Serialize)]
    struct Config {
        ident: String,
    }

    impl Default for Config {
        fn default() -> Config {
            Config {
                ident: NODE_ID.to_string(),
            }
        }
    }
    lazy_static! {
        static ref NODE_ID: String = random_name();
    }

    lazy_static! {
        static ref MAIN_CHAIN: Chain = Chain::new(genesis_blockchain());
    }

    let the_blockchain = Arc::new(Mutex::new(MAIN_CHAIN.clone()));

    //mines a new block and adds it to your node's chain
    #[get("/mine")]
    fn mine(the_blockchain: &State<Arc<Mutex<Chain>>>) -> Json<Block> {
        let blockchain = the_blockchain;
        let (blockchain_sender, blockchain_reciever) = channel();
        let (blockchain, blockchain_sender) = (Arc::clone(&blockchain), blockchain_sender.clone());
        thread::spawn(move || {
            let mut blockchain = blockchain.lock().unwrap();
            let mine_body =
                Transaction::build_transactions("NULL".to_string(), NODE_ID.to_string(), 1);
            let previous_hash = blockchain.blockchain.last_block.proof;
            let new_block = Block::new(
                blockchain.blockchain.last_block.index,
                mine_body,
                previous_hash.to_string(),
            );
            let blockchain_to_push = &blockchain.blockchain.clone();
            blockchain.add_chain(blockchain_to_push.clone());
            blockchain.blockchain.last_block = new_block;
            blockchain_sender.send(()).unwrap();
        });
        blockchain_reciever.recv().unwrap();
        let json_blockchain = the_blockchain.lock().unwrap().clone();
        Json(json_blockchain.blockchain.last_block)
    }

    //creates a new transaction from Json
    #[post("/transaction/new", data = "<body>")]
    fn new_transaction(body: Json<Transaction>) -> Json<Transaction> {
        Json(Transaction::build_transactions(
            body.sender.to_string(),
            body.recipiant.to_string(),
            body.amount,
        ))
    }

    //takes an Ip address and adds it to the node list
    #[post("/nodes/<nodes>")]
    fn nodes(nodes: String, chain: &State<Arc<Mutex<Chain>>>) {
        let nodes_str = from_str(&nodes).unwrap();
        chain.lock().unwrap().register_nodes(nodes_str);
    }

    //displays the full chain
    #[get("/chain")]
    fn full_chain(chain: &State<Arc<Mutex<Chain>>>) -> Json<Chain> {
        Json(chain.lock().unwrap().clone())
    }

    //comminucates with other nodes on the list to see who has the longest chain. Resolves an conflicts and adopts the longest chain.
    #[get("/resolve")]
    fn resolve_chain(chain: &State<Arc<Mutex<Chain>>>) -> Json<Chain> {
        let our_chain = chain;
        let (chain_sender, chain_reciever) = channel();
        let (chain, chain_sender) = (Arc::clone(&chain), chain_sender.clone());
        thread::spawn(move || {
            let mut chain = chain.lock().unwrap();
            chain.valid_chain();
            chain.resolve_conflicts();
            chain_sender.send(()).unwrap();
        });
        chain_reciever.recv().unwrap();
        Json(our_chain.lock().unwrap().clone())
    }

    rocket::custom(figment)
        .mount(
            "/",
            routes![mine, full_chain, new_transaction, nodes, resolve_chain],
        )
        .attach(AdHoc::config::<Config>())
        .manage(the_blockchain)
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Ident {
    node_id: String,
}

pub fn return_ident(figment: &Figment) -> String {
    // TODO should handle error instead of expect
    let create_info: String = figment.extract_inner("ident").expect("ident");
    create_info
}

//creates the initial blockchain
fn genesis_blockchain() -> Blockchain {
    let genesis_transaction =
        Transaction::build_transactions("null".to_string(), "null".to_string(), 0);
    let the_genesis_block = Block::new(0, genesis_transaction, 0.to_string());
    let default_transaction = &the_genesis_block.transactions.clone();
    let the_blockchain = Blockchain::new(the_genesis_block, default_transaction.clone());
    the_blockchain
}

#[derive(Debug, Serialize, Deserialize)]
struct NodeName {
    name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Chain {
    chain: Vec<Blockchain>,
    length: usize,
    blockchain: Blockchain,
    nodes: Vec<IpAddr>,
}
impl Chain {
    //initializes the blockchain list of nodes
    pub fn new(blockchain: Blockchain) -> Chain {
        let mut newchain: Vec<Blockchain> = Vec::new();
        let mut start_nodes: Vec<IpAddr> = Vec::new();
        let our_blockchain = blockchain.clone();
        //TODO make it so address is not hardcoded and instead read from Rocket.toml
        start_nodes.push(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
        newchain.push(blockchain);
        let new_length = newchain.len();
        Chain {
            chain: newchain,
            length: new_length,
            blockchain: our_blockchain,
            nodes: start_nodes,
        }
    }
    //adds a block to the chain
    pub fn add_chain(&mut self, blockchain: Blockchain) {
        self.chain.push(blockchain);
        self.length = self.length + 1;
    }

    //adds a node address to the vector of known nodes
    pub fn register_nodes(&mut self, address: &str) {
        let ip_to_push = IpAddr::from_str(address);
        // TODO handle if a bad IP is given
        let ip = ip_to_push.unwrap();
        if !self.nodes.contains(&ip) {
            self.nodes.push(ip);
        }
    }

    //verifies if the current chain matches the chains of other nodes
    pub fn valid_chain(&mut self) -> bool {
        let mut current_index: usize = 1;
        let mut last_block = self.chain[0].clone();
        let mut valid_chain_bool: bool = false;
        while current_index < self.chain.len() {
            let block = self.chain[current_index].clone();
            if block.last_block.previous_hash != last_block.last_block.previous_hash {
                valid_chain_bool = false;
            } else if last_block.last_block.proof != block.last_block.proof {
                valid_chain_bool = false;
            } else {
                last_block = block;
                valid_chain_bool = true;
            }
            current_index += 1;
        }
        valid_chain_bool
    }

    //corrects the current node's chain, if it is not the longest
    pub fn resolve_conflicts(&mut self) -> bool {
        let mut resolve_bool: bool = false;
        for node in self.nodes.clone() {
            let address = Url::parse(&format!("http://{:?}/chain", &node)).unwrap();
            let client = reqwest::blocking::Client::new();
            let json = client.get(address).send().unwrap().json::<JsonHolder>();
            let unwrapped_json = json.unwrap();
            let length = unwrapped_json.length;
            let chain = unwrapped_json.chain;

            if length > self.length && self.valid_chain() {
                self.length = length;
                self.chain = chain;
                resolve_bool = true;
            }
        }
        resolve_bool
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct JsonHolder {
    chain: Vec<Blockchain>,
    length: usize,
}

//finds the initial number that would be needed to create a hash that starts with zero.
pub fn valid_proof(input: &mut String, proof: u64) -> bool {
    let string_proof = proof.to_string();
    input.push_str(&string_proof);
    let mut hasher = Sha256::new();
    hasher.update(input);
    let result = hasher.finalize();
    let array_result: [u8; 32] = result.as_slice().try_into().expect("Wrong length");
    //array_result can be adjusted to alter the difficulty of mining a new block. For example: array_result[0..2] == [0,0]
    array_result[0..1] == [0]
}

//checks each input to see if it works for valid proof
pub fn proof_of_work(input: &mut String) -> u64 {
    let mut proof: u64 = 0;
    while valid_proof(input, proof) == false {
        proof += 1;
    }
    proof
}

//creates a random node name
fn random_name() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(7)
        .map(char::from)
        .collect()
}
#[derive(Debug, Serialize, Deserialize, Clone)]
struct Transaction {
    sender: String,
    recipiant: String,
    amount: u32,
}
impl Transaction {
    //creates a new transaction
    pub fn build_transactions(sender: String, recipiant: String, amount: u32) -> Transaction {
        Transaction {
            sender,
            recipiant,
            amount,
        }
    }
}
#[derive(Debug, Serialize, Deserialize, Clone)]
struct Block {
    index: u64,
    timestamp: DateTime<Utc>,
    transactions: Transaction,
    proof: u64,
    previous_hash: String,
}
impl Block {
    //creates a new block
    fn new(previous_index: u64, transactions: Transaction, mut previous_hash: String) -> Block {
        Block {
            index: previous_index + 1,
            timestamp: Utc::now(),
            transactions,
            proof: proof_of_work(&mut previous_hash),
            previous_hash,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Blockchain {
    current_transaction: Transaction,
    last_block: Block,
}

impl Blockchain {
    pub fn new(genesis_block: Block, current_transaction: Transaction) -> Blockchain {
        Blockchain {
            current_transaction,
            last_block: genesis_block,
        }
    }
}
