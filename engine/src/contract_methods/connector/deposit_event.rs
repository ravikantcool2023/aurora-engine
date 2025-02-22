use crate::contract_methods::connector::errors;
use crate::prelude::account_id::AccountId;
use crate::prelude::{
    format, vec, Address, BorshDeserialize, BorshSerialize, Fee, NEP141Wei, String, ToString, Vec,
    U256,
};
use aurora_engine_types::borsh;
use aurora_engine_types::parameters::connector::LogEntry;
use aurora_engine_types::types::address::error::AddressError;
use ethabi::{Event, EventParam, Hash, Log, ParamType, RawLog};

pub const DEPOSITED_EVENT: &str = "Deposited";

pub type EventParams = Vec<EventParam>;

/// On-transfer message. Used for `ft_transfer_call` and  `ft_on_transfer` functions.
/// Message parsed from input args with `parse_on_transfer_message`.
#[derive(BorshSerialize, BorshDeserialize)]
#[cfg_attr(not(target_arch = "wasm32"), derive(Debug, PartialEq, Eq))]
pub struct FtTransferMessageData {
    pub recipient: Address,
    #[deprecated]
    pub fee: Option<FtTransferFee>,
}

#[derive(BorshSerialize, BorshDeserialize)]
#[cfg_attr(not(target_arch = "wasm32"), derive(Debug, PartialEq, Eq))]
pub struct FtTransferFee {
    pub relayer: AccountId,
    pub amount: Fee,
}

impl FtTransferMessageData {
    /// Get on-transfer data from arguments message field.
    /// Used for `ft_transfer_call` and `ft_on_transfer`
    pub fn parse_on_transfer_message(
        message: &str,
    ) -> Result<Self, errors::ParseOnTransferMessageError> {
        if message.len() == 40 {
            // Parse message to determine recipient
            let recipient = {
                // Message format:
                //      Recipient of the transaction - 40 characters (Address in hex)
                let mut address_bytes = [0; 20];
                hex::decode_to_slice(message, &mut address_bytes)
                    .map_err(|_| errors::ParseOnTransferMessageError::InvalidHexData)?;
                Address::from_array(address_bytes)
            };

            #[allow(deprecated)]
            return Ok(Self {
                recipient,
                fee: None,
            });
        }

        // This logic is for backward compatibility to parse the message of the deprecated format.
        // "{relayer_id}:0000000000000000000000000000000000000000000000000000000000000000{hex_address}"

        // Split message by separator
        let (account, msg) = message
            .split_once(':')
            .ok_or(errors::ParseOnTransferMessageError::TooManyParts)?;

        // Check relayer account id from 1-th data element
        let account_id = account
            .parse()
            .map_err(|_| errors::ParseOnTransferMessageError::InvalidAccount)?;

        // Decode message array from 2-th element of data array
        // Length = fee[32] + eth_address[20] bytes
        let mut data = [0; 52];
        hex::decode_to_slice(msg, &mut data).map_err(|e| match e {
            hex::FromHexError::InvalidHexCharacter { .. } | hex::FromHexError::OddLength => {
                errors::ParseOnTransferMessageError::InvalidHexData
            }
            hex::FromHexError::InvalidStringLength => {
                errors::ParseOnTransferMessageError::WrongMessageFormat
            }
        })?;

        // Parse the fee from the message slice. It should contain 32 bytes,
        // but after that, it will be parsed to u128.
        // This logic is for compatibility.
        let fee_u128: u128 = U256::from_little_endian(&data[..32])
            .try_into()
            .map_err(|_| errors::ParseOnTransferMessageError::OverflowNumber)?;
        let fee_amount: Fee = fee_u128.into();

        // Get recipient Eth address from message slice
        let recipient = Address::try_from_slice(&data[32..]).unwrap();

        #[allow(deprecated)]
        Ok(Self {
            recipient,
            fee: Some(FtTransferFee {
                relayer: account_id,
                amount: fee_amount,
            }),
        })
    }

    /// Encode to String with specific rules
    #[must_use]
    pub fn encode(&self) -> String {
        #[allow(deprecated)]
        self.fee.as_ref().map_or_else(
            || hex::encode(self.recipient.as_bytes()),
            |fee| {
                // The first data section should contain fee data.
                // Pay attention, that for compatibility reasons we used U256 type.
                // It means 32 bytes for fee data and 20 bytes for address.
                let mut data = [0; 52];
                U256::from(fee.amount.as_u128()).to_little_endian(&mut data[..32]);
                // Second data section should contain Eth address.
                data[32..].copy_from_slice(self.recipient.as_bytes());
                // Add `:` separator between relayer_id and the data encoded in hex.
                format!("{}:{}", fee.relayer, hex::encode(data))
            },
        )
    }

    /// Prepare message for `ft_transfer_call` -> `ft_on_transfer`
    pub fn prepare_message_for_on_transfer(
        recipient: String,
    ) -> Result<Self, errors::ParseEventMessageError> {
        let address = if recipient.len() == 42 {
            recipient
                .strip_prefix("0x")
                .ok_or(errors::ParseEventMessageError::EthAddressValidationError(
                    AddressError::FailedDecodeHex,
                ))?
                .to_string()
        } else {
            recipient
        };

        let recipient_address = Address::decode(&address)
            .map_err(errors::ParseEventMessageError::EthAddressValidationError)?;

        #[allow(deprecated)]
        Ok(Self {
            recipient: recipient_address,
            fee: None,
        })
    }
}

/// Token message data used for Deposit flow.
/// It contains two basic data structure: Near, Eth
/// The message parsed from event `recipient` field - `log_entry_data`
/// after fetching proof `log_entry_data`
#[derive(BorshSerialize, BorshDeserialize)]
#[cfg_attr(not(target_arch = "wasm32"), derive(Debug, PartialEq, Eq))]
pub enum TokenMessageData {
    /// Deposit no NEAR account
    Near(AccountId),
    ///Deposit to Eth accounts fee is being minted in the `ft_on_transfer` callback method
    Eth {
        receiver_id: AccountId,
        message: FtTransferMessageData,
    },
}

impl TokenMessageData {
    /// Parse event message data for tokens. Data parsed form event `recipient` field.
    /// Used for Deposit flow.
    /// For Eth logic flow message validated and prepared for  `ft_on_transfer` logic.
    /// It means validating Eth address correctness and preparing message for
    /// parsing for `ft_on_transfer` message parsing with correct and validated data.
    pub fn parse_event_message_and_prepare_token_message_data(
        message: &str,
    ) -> Result<Self, errors::ParseEventMessageError> {
        let data: Vec<_> = message.split(':').collect();
        // Data array can contain 1 or 2 elements
        if data.len() >= 3 {
            return Err(errors::ParseEventMessageError::TooManyParts);
        }
        let account_id = AccountId::try_from(data[0].as_bytes())
            .map_err(|_| errors::ParseEventMessageError::InvalidAccount)?;

        // If data array contain only one element it should return NEAR account id
        if data.len() == 1 {
            Ok(Self::Near(account_id))
        } else {
            let raw_message = data[1].into();
            let message = FtTransferMessageData::prepare_message_for_on_transfer(raw_message)?;

            Ok(Self::Eth {
                receiver_id: account_id,
                message,
            })
        }
    }

    // Get recipient account id from Eth part of Token message data
    #[must_use]
    pub fn recipient(&self) -> AccountId {
        match self {
            Self::Near(acc) => acc.clone(),
            Self::Eth {
                receiver_id,
                message: _,
            } => receiver_id.clone(),
        }
    }
}

/// Ethereum event
pub struct EthEvent {
    pub eth_custodian_address: Address,
    pub log: Log,
}

#[allow(dead_code)]
impl EthEvent {
    /// Get Ethereum event from `log_entry_data`
    pub fn fetch_log_entry_data(
        name: &str,
        params: EventParams,
        data: &[u8],
    ) -> Result<Self, errors::DecodeError> {
        let event = Event {
            name: name.to_string(),
            inputs: params,
            anonymous: false,
        };
        let log_entry: LogEntry = rlp::decode(data).map_err(|_| errors::DecodeError::RlpFailed)?;
        let eth_custodian_address = Address::new(log_entry.address);
        let topics = log_entry.topics.iter().map(|h| Hash::from(h.0)).collect();

        let raw_log = RawLog {
            topics,
            data: log_entry.data,
        };
        let log = event
            .parse_log(raw_log)
            .map_err(|_| errors::DecodeError::SchemaMismatch)?;

        Ok(Self {
            eth_custodian_address,
            log,
        })
    }
}

/// Data that was emitted by Deposited event.
#[cfg_attr(not(target_arch = "wasm32"), derive(Debug, PartialEq, Eq))]
pub struct DepositedEvent {
    pub eth_custodian_address: Address,
    pub sender: Address,
    pub token_message_data: TokenMessageData,
    pub amount: NEP141Wei,
    pub fee: Fee,
}

impl DepositedEvent {
    #[allow(dead_code)]
    #[must_use]
    pub fn event_params() -> EventParams {
        vec![
            EventParam {
                name: "sender".to_string(),
                kind: ParamType::Address,
                indexed: true,
            },
            EventParam {
                name: "recipient".to_string(),
                kind: ParamType::String,
                indexed: false,
            },
            EventParam {
                name: "amount".to_string(),
                kind: ParamType::Uint(256),
                indexed: false,
            },
            EventParam {
                name: "fee".to_string(),
                kind: ParamType::Uint(256),
                indexed: false,
            },
        ]
    }

    /// Parses raw Ethereum logs proof's entry data
    pub fn from_log_entry_data(data: &[u8]) -> Result<Self, errors::ParseError> {
        let event = EthEvent::fetch_log_entry_data(DEPOSITED_EVENT, Self::event_params(), data)
            .map_err(errors::ParseError::LogParseFailed)?;
        let raw_sender = event.log.params[0]
            .value
            .clone()
            .into_address()
            .ok_or(errors::ParseError::InvalidSender)?
            .0;
        let sender = Address::from_array(raw_sender);

        // parse_event_message
        let event_message_data: String = event.log.params[1].value.clone().to_string();

        let amount = event.log.params[2]
            .value
            .clone()
            .into_uint()
            .ok_or(errors::ParseError::InvalidAmount)?
            .try_into()
            .map(NEP141Wei::new)
            .map_err(|_| errors::ParseError::OverflowNumber)?;
        let fee = event.log.params[3]
            .value
            .clone()
            .into_uint()
            .ok_or(errors::ParseError::InvalidFee)?
            .try_into()
            .map(|v| Fee::new(NEP141Wei::new(v)))
            .map_err(|_| errors::ParseError::OverflowNumber)?;

        let token_message_data =
            TokenMessageData::parse_event_message_and_prepare_token_message_data(
                &event_message_data,
            )?;

        Ok(Self {
            eth_custodian_address: event.eth_custodian_address,
            sender,
            token_message_data,
            amount,
            fee,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors;
    use aurora_engine_types::types::address::make_address;
    use aurora_engine_types::H160;

    #[test]
    fn test_decoded_and_then_encoded_message_does_not_change() {
        let expect_message =
            "aurora:05000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";
        let message_data =
            FtTransferMessageData::parse_on_transfer_message(expect_message).unwrap();
        let actual_message = message_data.encode();

        assert_eq!(expect_message, actual_message);
    }

    #[test]
    fn test_decoded_and_then_encoded_message_without_fee_does_not_change() {
        let expect_message = "000000000000000000000000000000000000dead";
        let message_data =
            FtTransferMessageData::parse_on_transfer_message(expect_message).unwrap();
        let actual_message = message_data.encode();

        assert_eq!(expect_message, actual_message);
    }

    #[test]
    fn test_parsing_message_with_incorrect_amount_of_parts() {
        let message = "foo";
        let error = FtTransferMessageData::parse_on_transfer_message(message).unwrap_err();
        let expected_error_message = errors::ERR_INVALID_ON_TRANSFER_MESSAGE_FORMAT;
        let actual_error_message = error.as_ref();

        assert_eq!(expected_error_message, actual_error_message);
    }

    #[test]
    fn test_parsing_message_with_invalid_account_id() {
        let message = "INVALID:0";
        let error = FtTransferMessageData::parse_on_transfer_message(message).unwrap_err();
        let expected_error_message = errors::ERR_INVALID_ACCOUNT_ID;
        let actual_error_message = error.as_ref();

        assert_eq!(expected_error_message, actual_error_message);
    }

    #[test]
    fn test_parsing_message_with_invalid_hex_data() {
        let message = "foo:INVALID";
        let error = FtTransferMessageData::parse_on_transfer_message(message).unwrap_err();
        let expected_error_message = errors::ERR_INVALID_ON_TRANSFER_MESSAGE_HEX;
        let actual_error_message = error.as_ref();

        assert_eq!(expected_error_message, actual_error_message);
    }

    #[test]
    fn test_parsing_message_without_fee_with_invalid_hex_data() {
        let message = "g00000000000000000000000000000000000dead";
        let error = FtTransferMessageData::parse_on_transfer_message(message).unwrap_err();
        let expected_error_message = errors::ERR_INVALID_ON_TRANSFER_MESSAGE_HEX;
        let actual_error_message = error.as_ref();

        assert_eq!(expected_error_message, actual_error_message);
    }

    #[test]
    fn test_parsing_message_with_invalid_length_of_hex_data() {
        let message = "foo:dead";
        let error = FtTransferMessageData::parse_on_transfer_message(message).unwrap_err();
        let expected_error_message = errors::ERR_INVALID_ON_TRANSFER_MESSAGE_DATA;
        let actual_error_message = error.as_ref();

        assert_eq!(expected_error_message, actual_error_message);
    }

    #[test]
    fn test_parsing_message_with_overflowing_fee() {
        let message =
            "foo:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
        let error = FtTransferMessageData::parse_on_transfer_message(message).unwrap_err();
        let expected_error_message = errors::ERR_OVERFLOW_NUMBER;
        let actual_error_message = error.as_ref();

        assert_eq!(expected_error_message, actual_error_message);
    }

    #[test]
    fn test_eth_token_message_data_decodes_recipient_correctly() {
        let address = Address::zero();
        let message = format!("aurora:{}", address.encode());

        let token_message_data =
            TokenMessageData::parse_event_message_and_prepare_token_message_data(&message).unwrap();
        let actual_recipient = token_message_data.recipient().to_string();
        let expected_recipient = "aurora";

        assert_eq!(expected_recipient, actual_recipient);
    }

    #[test]
    fn test_eth_token_message_data_decodes_recipient_correctly_with_prefix() {
        let address = Address::zero();
        let message = format!("aurora:0x{}", address.encode());

        let token_message_data =
            TokenMessageData::parse_event_message_and_prepare_token_message_data(&message).unwrap();
        let actual_recipient = token_message_data.recipient().to_string();
        let expected_recipient = "aurora";

        assert_eq!(expected_recipient, actual_recipient);
    }

    #[test]
    fn test_near_token_message_data_decodes_recipient_correctly() {
        let message = "aurora";

        let token_message_data =
            TokenMessageData::parse_event_message_and_prepare_token_message_data(message).unwrap();
        let actual_recipient = token_message_data.recipient().to_string();
        let expected_recipient = "aurora";

        assert_eq!(expected_recipient, actual_recipient);
    }

    #[test]
    fn test_token_message_data_fails_with_too_many_parts() {
        let message = "aurora:foo:bar";

        let parse_error =
            TokenMessageData::parse_event_message_and_prepare_token_message_data(message)
                .unwrap_err();
        let actual_parse_error = parse_error.as_ref();
        let expected_parse_error = errors::ERR_INVALID_EVENT_MESSAGE_FORMAT;

        assert_eq!(expected_parse_error, actual_parse_error);
    }

    #[test]
    fn test_token_message_data_fails_with_invalid_account() {
        let message = "INVALID";

        let parse_error =
            TokenMessageData::parse_event_message_and_prepare_token_message_data(message)
                .unwrap_err();
        let actual_parse_error = parse_error.as_ref();
        let expected_parse_error = errors::ERR_INVALID_ACCOUNT_ID;

        assert_eq!(expected_parse_error, actual_parse_error);
    }

    #[test]
    fn test_eth_token_message_data_fails_with_invalid_address_length() {
        let message = "aurora:0xINVALID";

        let parse_error =
            TokenMessageData::parse_event_message_and_prepare_token_message_data(message)
                .unwrap_err();
        let actual_parse_error = std::str::from_utf8(parse_error.as_ref()).unwrap();
        let expected_parse_error = AddressError::IncorrectLength.to_string();

        assert_eq!(expected_parse_error, actual_parse_error);
    }

    #[test]
    fn test_eth_token_message_data_fails_with_invalid_address() {
        let message = "aurora:0xINVALID_ADDRESS_WITH_CORRECT_LENGTH_HERE";

        let parse_error =
            TokenMessageData::parse_event_message_and_prepare_token_message_data(message)
                .unwrap_err();
        let actual_parse_error = std::str::from_utf8(parse_error.as_ref()).unwrap();
        let expected_parse_error = AddressError::FailedDecodeHex.to_string();

        assert_eq!(expected_parse_error, actual_parse_error);
    }

    #[test]
    fn test_deposited_event_parses_from_log_entry_successfully() {
        let recipient_address = Address::zero();
        let eth_custodian_address = make_address(0xd045f7e1, 0x9b2488924b97f9c145b5e51d0d895a65);

        let fee = Fee::new(NEP141Wei::new(0));
        let message = ["aurora", ":", recipient_address.encode().as_str()].concat();
        let token_message_data: TokenMessageData =
            TokenMessageData::parse_event_message_and_prepare_token_message_data(&message).unwrap();

        let expected_deposited_event = DepositedEvent {
            eth_custodian_address,
            sender: Address::new(H160([0u8; 20])),
            token_message_data,
            amount: NEP141Wei::new(0),
            fee,
        };

        let event_schema = Event {
            name: DEPOSITED_EVENT.into(),
            inputs: DepositedEvent::event_params(),
            anonymous: false,
        };
        let log_entry = LogEntry {
            address: eth_custodian_address.raw(),
            topics: vec![
                event_schema.signature(),
                // the sender is not important
                crate::prelude::H256::zero(),
            ],
            data: ethabi::encode(&[
                ethabi::Token::String(message),
                ethabi::Token::Uint(U256::from(expected_deposited_event.amount.as_u128())),
                ethabi::Token::Uint(U256::from(expected_deposited_event.fee.as_u128())),
            ]),
        };

        let log_entry_data = rlp::encode(&log_entry).to_vec();
        let actual_deposited_event = DepositedEvent::from_log_entry_data(&log_entry_data).unwrap();

        assert_eq!(expected_deposited_event, actual_deposited_event);
    }
}
