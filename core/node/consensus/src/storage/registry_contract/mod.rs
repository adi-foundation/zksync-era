use anyhow::Context;
use zksync_concurrency::{ctx, error::Wrap as _};
use zksync_contracts::consensus_l2_contracts as contracts;
use zksync_consensus_roles::{validator,attester};
use zksync_consensus_crypto::ByteFmt;
use zksync_node_api_server::{
    execution_sandbox::{BlockArgs, BlockStartInfo},
    tx_sender::TxSender,
};
use zksync_system_constants::DEFAULT_L2_TX_GAS_PER_PUBDATA_BYTE;
use zksync_types::{
    api,
    ethabi,
    fee::Fee,
    l2::L2Tx,
    transaction_request::CallOverrides,
    Nonce, U256,
};
use crate::storage::Connection;

#[cfg(test)]
mod tests;

/// A struct for reading data from consensus L2 contracts.
#[derive(Debug)]
pub(crate) struct VM {
    contract: contracts::ConsensusRegistry,
    address: ethabi::Address,
    sender: TxSender,
}

pub(crate) struct AddInputs {
    node_owner: ethabi::Address,
    validator: WeightedValidator,
    attester: attester::WeightedAttester,
}

pub(crate) struct WeightedValidator {
    weight: validator::Weight,
    key: validator::PublicKey,
    pop: validator::ProofOfPossession,
}

impl AddInputs {
    fn encode(&self) -> anyhow::Result<contracts::AddInputs> {
        Ok(contracts::AddInputs {
            node_owner: self.node_owner,
            validator_pub_key: encode_validator_key(&self.validator.key),
            validator_weight: self.validator.weight.into(),
            validator_pop: encode_validator_pop(&self.validator.pop),
            attester_pub_key: encode_attester_key(&self.attester.key),
            attester_weight: self.attester.weight.try_into().context("overflow")?,
        })
    }
}

fn encode_attester_key(k : &attester::PublicKey) -> contracts::Secp256k1PublicKey {
    let b: [u8;33] = ByteFmt::encode(&k).try_into().unwrap();
    Ok(contracts::Secp256k1PublicKey {
        tag: b[0],
        x: b[1..33],
    })
}

fn decode_attester_key(k: &contracts::Secp256k1PublicKey) -> anyhow::Result<attester::PublicKey> {
    let mut x = vec![k.tag];
    x.extend(k.x);
    ByteFmt::decode(&x) 
}

fn encode_validator_key(k: &validator::PublicKey) -> contracts::BLS12_381PublicKey {
    let b: [u8;96] = ByteFmt::encode(k).try_into().unwrap();
    contracts::BLS12_381PublicKey {
        a: b[0..32],
        b: b[32..64],
        c: b[64..96],
    }
}

fn encode_validator_pop(pop: &validator::ProofOfPossession) -> contracts::BLS12_381Signature {
    let b: [u8;48] = ByteFmt::encode(pop).try_into().unwrap();
    contracts::BLS12_381Signature {
        a: b[0..32],
        b: b[32..48],
    }
}

/*
fn decode_validator_key(k: contracts::BLS12_381PublicKey) -> anyhow::Result<validator::PublicKey> {
    let mut x = Vec::from(k.a);
    x.extend(k.b);
    x.extend(k.c);
    ByteFmt::decode(&x)
}

fn encode_weighted_attester(a: attester::WeightedAttester) -> anyhow::Result<contracts::Attester> {
    Ok(contracts::Attester {
        weight: a.weight.try_into().context("overflow")?,
        pub_key: encode_validator_key(&a.key),
    })
}*/

fn decode_weighted_attester(a: &contracts::Attester) -> anyhow::Result<attester::WeightedAttester> {
    Ok(attester::WeightedAttester {
        weight: a.weight.into(),
        key: decode_attester_key(&a.pub_key).context("key")?,
    })
}

type Calldata = Vec<u8>;

impl VM {
    /// Constructs a new `VMReader` instance.
    pub fn new(tx_sender: TxSender, address: ethabi::Address) -> Self {
        Self {
            tx_sender,
            contract: contracts::ConsensusRegistry::load(),
            address,
        }
    }

    /// Reads attester committee from the registry contract.
    /// It's implemented by dispatching multiple read transactions (a.k.a. `eth_call` requests),
    /// each one carries an instantiation of a separate VM execution sandbox.
    pub async fn get_attester_committee(&self, ctx: &ctx::Ctx, conn: &mut Connection<'_>, block: api::BlockId) -> anyhow::Result<attester::Committee> {
        let raw = self.call(ctx, conn, block, self.contract.get_attester_commitee(), ())?;
        let mut attesters = vec![];
        for a in raw {
           attesters.push(decode_weighted_attester(&a).context("decode_weighted_attester()")?);
        }
        attester::Committee::new(attesters.into_iter()).context("Committee::new()")
    }

    async fn eth_call<Sig: contracts::FunctionSig>(&self, f: contracts::Function<'_, Sig>, input: Sig::Inputs) -> anyhow::Result<L2Tx> {
    fn eth_call(ctx: &ctx::Ctx, conn: &mut Connection<'_>, batch: attester::BatchNumber, tx: L2Tx) -> Bytes {
        L2Tx::new(
            self.address,
            f.encode_input(input).context("encode_input")?,
            Nonce(0),
            Fee {
                gas_limit: U256::from(2000000000u32),
                max_fee_per_gas: U256::zero(),
                max_priority_fee_per_gas: U256::zero(),
                gas_per_pubdata_limit: U256::from(DEFAULT_L2_TX_GAS_PER_PUBDATA_BYTE),
            },
            ethabi::Address::zero(),
            U256::zero(),
            vec![],
            Default::default(),
        )
        let overrides = CallOverrides { enforced_base_fee: None };
        let (_, block) = conn.get_l2_block_range_of_l1_batch(ctx, batch).await.context("get_l2_block_range_of_l1_batch()")?.context("batch not sealed")?;
        let block = api::BlockId::Number(api::BlockNumber::Number(block.0.into()));
        let start_info = BlockStartInfo::new(&mut conn, /*max_cache_age=*/ std::time::Duration::from_secs(10)).await.unwrap();
        let args = BlockArgs::new(&mut conn, block, &start_info).await.context("BlockArgs::new")
        let output = self.tx_sender.eth_call(args, overrides, tx, None).await.context("tx_sender.eth_call()");
        output.parse()
    }
}
