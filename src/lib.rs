use alloy_primitives::{address, U256};
use alloy_sol_types::{sol, SolCall};
use cartesi_machine::{
    cartesi_machine_sys::{cm_concurrency_runtime_config, cm_htif_runtime_config},
    configuration::{MemoryRangeConfig, RuntimeConfig},
    Machine,
};
use std::ffi::CString;
use std::rc::Rc;

use std::fs::File;
use std::{
    collections::HashMap,
    io::{Error, ErrorKind},
};
pub mod hash;
mod merkle_tree;
pub mod proofs;

const HTIF_YIELD_REASON_ADVANCE_STATE_DEF: u16 = 0;
const HTIF_YIELD_REASON_TX_REPORT_DEF: u16 = 0x4;
const HTIF_YIELD_REASON_TX_OUTPUT_DEF: u16 = 0x1;
const PMA_CMIO_TX_BUFFER_START_DEF: u64 = 0x60800000;

const MEMORY_RANGE_CONFIG_START: u64 = 0x90000000000000;
const M16: u64 = (1 << 16) - 1;
const M32: u64 = (1 << 32) - 1;
pub fn run_advance(
    machine_snapshot: String,
    lambda_state_previous: &str,
    lambda_state_next: &str,
    payload: Vec<u8>,
    metadata: HashMap<Vec<u8>, Vec<u8>>,
    report_callback: Box<dyn Fn(u16, &[u8]) -> Result<(u16, Vec<u8>), Error>>,
    output_callback: &mut Box<impl FnMut(u16, &[u8]) -> Result<(u16, Vec<u8>), Error>>,
    callbacks: HashMap<u32, Box<dyn Fn(u16, &[u8]) -> Result<(u16, Vec<u8>), Error>>>,
    no_console_putchar: bool,
) -> Result<(), Error> {
    match reflink::reflink_or_copy(lambda_state_previous, lambda_state_next) {
        Ok(Some(_)) => {
            eprintln!("WARNING: could not reflink lambda state, copying instead");
        }
        Ok(None) => {}
        Err(e) => return Err(e),
    }

    let lambda_state_previous_file = File::open(lambda_state_previous).unwrap();
    let lambda_state_previous_file_size = lambda_state_previous_file.metadata().unwrap().len();

    let mut machine = Machine::load(
        std::path::Path::new(machine_snapshot.as_str()),
        RuntimeConfig {
            values: cartesi_machine::cartesi_machine_sys::cm_machine_runtime_config {
                skip_root_hash_check: true,
                skip_root_hash_store: true,
                concurrency: cm_concurrency_runtime_config {
                    update_merkle_tree: 0,
                },
                htif: cm_htif_runtime_config {
                    no_console_putchar: false,
                },
                skip_version_check: false,
                soft_yield: false,
            },
        },
    )
    .unwrap();
    let cs_filename = CString::new(lambda_state_next.to_string()).unwrap();
    let mut cs_filename_bytes: Vec<u8> = cs_filename.into_bytes();
    let filename_pointer: *const i8 = cs_filename_bytes.as_mut_ptr() as *const i8;
    machine
        .replace_memory_range(&MemoryRangeConfig {
            start: MEMORY_RANGE_CONFIG_START,
            length: lambda_state_previous_file_size,
            shared: true,
            image_filename: filename_pointer,
        })
        .unwrap();

    let mut data = machine.read_htif_tohost_data().unwrap();
    let mut reason = ((data >> 32) & M16) as u16;

    if reason == HTIF_YIELD_REASON_ADVANCE_STATE_DEF {
        let payload = payload;
        let encoded = encode_evm_advance(payload);
        machine
            .send_cmio_response(HTIF_YIELD_REASON_ADVANCE_STATE_DEF, &encoded)
            .unwrap();
        //TODO send gio response with metadata etc.

        machine.reset_iflags_y().unwrap();
    } else {
        return Err(Error::new(
            ErrorKind::Other,
            format!("current reason is {:?}, but 0 was expected", reason),
        ));
    }

    let max_cycles = u64::MAX;

    loop {
        if !machine.read_iflags_y().unwrap() {
            let _ = Some(machine.run(max_cycles).unwrap());
        }
        data = machine.read_htif_tohost_data().unwrap();
        reason = ((data >> 32) & M16) as u16;
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
            _ => match callbacks.get(&(reason as u32)) {
                Some(unknown_gio_callback) => {
                    unknown_gio_callback(reason, &data).unwrap();
                }
                None => {
                    println!("No callback found");
                    drop(machine);
                    break;
                }
            },
        }
        machine.reset_iflags_y().unwrap();
    }
    return Ok(());
}

fn encode_evm_advance(payload: Vec<u8>) -> Vec<u8> {
    sol! { interface Inputs {
        function EvmAdvance(
            uint256 chainId,
            address appContract,
            address msgSender,
            uint256 blockNumber,
            uint256 blockTimestamp,
            uint256 prevRandao,
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
        prevRandao: U256::from(0),
        index: U256::from(0),
        payload: payload.into(),
    };
    call.abi_encode()
}
