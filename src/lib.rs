use alloy_primitives::{address, U256};
use alloy_sol_types::{sol, SolCall};
use cartesi_machine::{
    configuration::{MemoryRangeConfig, RuntimeConfig},
    Machine,
};
use std::{collections::HashMap, io::Error};
const HTIF_YIELD_REASON_ADVANCE_STATE_DEF: u16 = 0;
const HTIF_YIELD_REASON_TX_REPORT_DEF: u16 = 0x4;
const HTIF_YIELD_REASON_TX_OUTPUT_DEF: u16 = 0x1;
const HTIF_YIELD_REASON_TX_VOUCHER_DEF: u16 = 0x2;

const PMA_CMIO_TX_BUFFER_START_DEF: u64 = 0x60800000;

const MEMORY_RANGE_CONFIG_LENGTH: u64 = 4096;
const MEMORY_RANGE_CONFIG_START: u64 = 0x90000000000000;

fn advance_runner(
    machine_snapshot: String,
    lambda_state_previous: &str,
    lambda_state_next: &str,
    payload: Vec<u8>,
    metadata: HashMap<Vec<u8>, Vec<u8>>,
    report_callback: Box<dyn Fn(u16, &[u8]) -> Result<(u16, Vec<u8>), Error>>,
    output_callback: Box<dyn Fn(u16, &[u8]) -> Result<(u16, Vec<u8>), Error>>,
    callbacks: HashMap<u32, Box<dyn Fn(u16, &[u8]) -> Result<(u16, Vec<u8>), Error>>>,
) {
    reflink::reflink(lambda_state_previous, lambda_state_next).unwrap();
    let mut machine = Machine::load(
        std::path::Path::new(machine_snapshot.as_str()),
        RuntimeConfig {
            skip_root_hash_check: true,
            skip_root_hash_store: true,
            ..Default::default()
        },
    )
    .unwrap();

    machine
        .replace_memory_range(MemoryRangeConfig {
            start: MEMORY_RANGE_CONFIG_START,
            length: MEMORY_RANGE_CONFIG_LENGTH,
            shared: true,
            image_filename: Some(lambda_state_next.to_string()),
        })
        .unwrap();

    let payload = payload;
    let encoded = encode_evm_advance(payload);
    machine
        .send_cmio_response(HTIF_YIELD_REASON_ADVANCE_STATE_DEF, &encoded)
        .unwrap();

    //TODO send gio response with metadata etc.

    let max_cycles = u64::MAX;
    let _ = Some(machine.run(max_cycles).unwrap());
    let data = machine.read_htif_tohost_data().unwrap();
    const M16: u64 = (1 << 16) - 1;
    const M32: u64 = (1 << 32) - 1;
    let reason = ((data >> 32) & M16) as u16;
    let length = data & M32; // length
    let data = machine
        .read_memory(PMA_CMIO_TX_BUFFER_START_DEF, length)
        .unwrap();
    match reason {
        HTIF_YIELD_REASON_TX_REPORT_DEF => {
            report_callback(reason, &data).unwrap();
        }
        HTIF_YIELD_REASON_TX_OUTPUT_DEF => {
            output_callback(reason, &data).unwrap();
        }
        HTIF_YIELD_REASON_TX_VOUCHER_DEF => {}
        _ => {
            match callbacks.get(&(reason as u32)) {
                Some(unknown_gio_callback) => {
                    unknown_gio_callback(reason, &data).unwrap();
                }
                None => {
                    println!("No callback found");
                }
            }
            drop(machine);
        }
    }
}

fn encode_evm_advance(payload: Vec<u8>) -> Vec<u8> {
    sol! { interface Inputs {
        function EvmAdvance(
            uint256 chainId,
            address appContract,
            address msgSender,
            uint256 blockNumber,
            uint256 blockTimestamp,
            uint256 index,
            bytes calldata payload
        ) external;
    } };
    let call = Inputs::EvmAdvanceCall {
        chainId: U256::from(0),
        appContract: address!(),
        msgSender: address!(),
        blockNumber: U256::from(0),
        blockTimestamp: U256::from(0),
        index: U256::from(0),
        payload: payload.into(),
    };
    call.abi_encode()
}
