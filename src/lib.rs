use alloy_primitives::{address, U256};
use alloy_sol_types::{sol, SolCall};
use cartesi_machine::{
    cartesi_machine_sys::{
        CM_CMIO_YIELD_REASON_ADVANCE_STATE, CM_CMIO_YIELD_REASON_INSPECT_STATE, CM_REG_IFLAGS_Y,
    },
    config::runtime::{ConcurrencyRuntimeConfig, HTIFRuntimeConfig, RuntimeConfig},
    constants::cmio::{commands, tohost::manual::RX_ACCEPTED},
    machine::Machine,
    types::cmio::{AutomaticReason, CmioRequest, CmioResponseReason, ManualReason},
};
use std::error::Error;
use std::fs::File;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::{collections::HashMap, io::ErrorKind};
pub mod hash;
mod merkle_tree;
pub mod proofs;

const MEMORY_RANGE_CONFIG_START: u64 = 0x90000000000000;
#[derive(PartialEq)]
pub enum YieldManualReason {
    Accepted,
    Rejected,
    Exception,
}
pub struct RunAdvanceLambdaStatePaths {
    pub lambda_state_previous_path: String,
    pub lambda_state_next_path: String,
}
pub async fn run_advance(
    machine_snapshot: String,
    lambda_state_paths: Option<RunAdvanceLambdaStatePaths>,
    payload: Vec<u8>,
    metadata: HashMap<Vec<u8>, Vec<u8>>,
    report_callback: &mut impl FnMut(u16, &[u8]) -> Result<(u16, Vec<u8>), Box<dyn Error>>,
    output_callback: &mut impl FnMut(u16, &[u8]) -> Result<(u16, Vec<u8>), Box<dyn Error>>,
    finish_callback: &mut impl FnMut(u16, &[u8]) -> Result<(u16, Vec<u8>), Box<dyn Error>>,
    callbacks: HashMap<u32, Callback>,
    no_console_putchar: bool,
) -> Result<YieldManualReason, Box<dyn Error>> {
    if let Some(lambda_state_paths) = &lambda_state_paths {
        match reflink::reflink_or_copy(
            &lambda_state_paths.lambda_state_previous_path,
            &lambda_state_paths.lambda_state_next_path,
        ) {
            Ok(Some(_)) => {
                eprintln!("WARNING: could not reflink lambda state, copying instead");
            }
            Ok(None) => {}
            Err(e) => return Err(Box::new(e)),
        }
    }

    let mut machine = Machine::load(
        std::path::Path::new(machine_snapshot.as_str()),
        &RuntimeConfig {
            skip_root_hash_check: Some(true),
            skip_root_hash_store: Some(true),
            concurrency: Some(ConcurrencyRuntimeConfig {
                update_merkle_tree: Some(0),
            }),
            htif: Some(HTIFRuntimeConfig {
                no_console_putchar: Some(no_console_putchar),
            }),
            skip_version_check: Some(false),
            soft_yield: Some(false),
        },
    )
    .unwrap();
    if let Some(lambda_state_paths) = lambda_state_paths {
        let lambda_state_previous_file =
            File::open(lambda_state_paths.lambda_state_previous_path).unwrap();
        let lambda_state_previous_file_size = lambda_state_previous_file.metadata().unwrap().len();
        let filename = Path::new(&lambda_state_paths.lambda_state_next_path);
        machine
            .replace_memory_range(
                MEMORY_RANGE_CONFIG_START,
                lambda_state_previous_file_size,
                true,
                Some(filename),
            )
            .unwrap();
    }

    let cmdio = machine.receive_cmio_request().unwrap();

    if cmdio.reason() == RX_ACCEPTED && cmdio.cmd() == commands::YIELD_MANUAL {
        let payload = payload;
        let encoded = encode_evm_advance(payload);
        machine
            .send_cmio_response(CmioResponseReason::Advance, &encoded)
            .unwrap();
        //TODO send gio response with metadata etc.

        machine.write_reg(CM_REG_IFLAGS_Y, 0)?;
    } else {
        return Err(Box::new(std::io::Error::new(
            ErrorKind::Other,
            format!("current reason is {:?}, but 0 was expected", cmdio.reason()),
        )));
    }

    let max_cycles = u64::MAX;
    loop {
        if !machine.iflags_y().unwrap() {
            let _ = Some(machine.run(max_cycles).unwrap());
        }
        let cmdio = machine.receive_cmio_request().unwrap();
        let reason = cmdio.reason();
        match cmdio {
            CmioRequest::Automatic(automatic_reason) => (match automatic_reason {
                AutomaticReason::TxReport { data } => {
                    report_callback(reason, &data)?;
                }
                AutomaticReason::TxOutput { data } => {
                    output_callback(reason, &data)?;
                }
                _ => {
                    return Err(Box::new(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("unknown reason {:?}", reason),
                    )));
                }
            },),
            CmioRequest::Manual(manual_reason) => (match manual_reason {
                ManualReason::RxAccepted {
                    output_hashes_root_hash,
                } => {
                    finish_callback(reason, &output_hashes_root_hash)?;
                    return Ok(YieldManualReason::Accepted);
                }
                ManualReason::RxRejected => {
                    finish_callback(reason, vec![])?;
                    return Ok(YieldManualReason::Rejected);
                }
                ManualReason::TxException { message } => {
                    finish_callback(reason, message.as_bytes().to_vec())?;
                    return Ok(YieldManualReason::Exception);
                }
                ManualReason::GIO { domain: _, data } => {
                    match callbacks.get(&(reason as u32)) {
                        Some(unknown_gio_callback) => {
                            let callback_output = match unknown_gio_callback {
                                Callback::Sync(sync_callback) => sync_callback(reason, data)?,
                                Callback::Async(async_callback) => {
                                    async_callback(reason, data).await?
                                }
                            };
                            let cmdio_send_reason = match reason as u32 {
                                CM_CMIO_YIELD_REASON_ADVANCE_STATE => CmioResponseReason::Advance,
                                CM_CMIO_YIELD_REASON_INSPECT_STATE => CmioResponseReason::Inspect,
                                _ => {
                                    return Err(Box::new(std::io::Error::new(
                                        std::io::ErrorKind::Other,
                                        format!("Unknown cmdio reason"),
                                    )));
                                }
                            };
                            machine
                                .send_cmio_response(cmdio_send_reason, &callback_output)
                                .unwrap();
                        }
                        None => {
                            println!("No callback found");
                            drop(machine);
                            return Err(Box::new(std::io::Error::new(
                                std::io::ErrorKind::Other,
                                format!("No callback found"),
                            )));
                        }
                    };
                }
            },),
        };
        machine.write_reg(CM_REG_IFLAGS_Y, 0)?;
    }
}
pub enum Callback {
    Sync(Box<dyn Fn(u16, Vec<u8>) -> Result<Vec<u8>, Box<dyn Error>>>),
    Async(
        Box<dyn Fn(u16, Vec<u8>) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, Box<dyn Error>>>>>>,
    ),
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
