#![allow(clippy::collapsible_else_if)]

#[macro_use]
extern crate ic_cdk_macros;
#[macro_use]
extern crate serde;

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::convert::TryFrom;
use std::iter::FromIterator;
use std::mem;
use std::num::TryFromIntError;
use std::result::Result as StdResult;

use candid::{CandidType, Encode, Principal};
use chrono::{Utc, TimeZone};
use ic_cdk::{
    api::{self, call},
    export::candid,
    storage,
};
use ic_certified_map::Hash;
use include_base64::include_base64;

use sha2::{Digest, Sha256};
use uriparse::URI;

mod http;


const MGMT: Principal = Principal::from_slice(&[]);

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

#[derive(CandidType, Deserialize)]
struct StableState {
    state: State,
    hashes: Vec<(String, Hash)>,
}

#[pre_upgrade]
fn pre_upgrade() {
    let state = STATE.with(|state| mem::take(&mut *state.borrow_mut()));
    let hashes = http::HASHES.with(|hashes| mem::take(&mut *hashes.borrow_mut()));
    let hashes = hashes.iter().map(|(k, v)| (k.clone(), *v)).collect();
    let stable_state = StableState { state, hashes };
    storage::stable_save((stable_state,)).unwrap();
}
#[post_upgrade]
fn post_upgrade() {
    let (StableState { state, hashes },) = storage::stable_restore().unwrap();
    STATE.with(|state0| *state0.borrow_mut() = state);
    let hashes = hashes.into_iter().collect();
    http::HASHES.with(|hashes0| *hashes0.borrow_mut() = hashes);
}

#[derive(CandidType, Deserialize)]
struct InitArgs {
    custodians: Option<HashSet<Principal>>,
    logo: Option<LogoResult>,
    name: String,
    symbol: String,
    white_list: Vec<Principal>,
    begin_date: String,
    end_date: String,
    total_limit: String,
}

#[init]
fn init(args: InitArgs) {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.custodians = args
            .custodians
            .unwrap_or_else(|| HashSet::from_iter([api::caller()]));
        state.name = args.name;
        state.symbol = args.symbol;
        state.logo = args.logo;
        state.white_list = args.white_list;
        state.total_limit = args.total_limit;
        let fmt = "%Y-%m-%d %H:%M:%S";
        
        match Utc.datetime_from_str(&args.begin_date, fmt){
            Ok(_) => {
                state.begin_date = args.begin_date
            },
            Err(e) => {
                panic!("{}",e)
            },
        };
        
        match Utc.datetime_from_str(&args.end_date, fmt){
            Ok(_) => {
                state.end_date = args.end_date
            },
            Err(e) => {
                panic!("{}",e)
            },
        };

    });
}

#[derive(CandidType, Deserialize)]
enum Error {
    Unauthorized,
    InvalidTokenId,
    ZeroAddress,
    Other,
}

impl From<TryFromIntError> for Error {
    fn from(_: TryFromIntError) -> Self {
        Self::InvalidTokenId
    }
}

type Result<T = u128, E = Error> = StdResult<T, E>;

// --------------
// base interface
// --------------

#[query(name = "balanceOfDip721")]
fn balance_of(user: Principal) -> u64 {
    STATE.with(|state| {
        state
            .borrow()
            .nfts
            .iter()
            .filter(|n| n.owner == user)
            .count() as u64
    })
}

#[query(name = "ownerOfDip721")]
fn owner_of(token_id: u64) -> Result<Principal> {
    STATE.with(|state| {
        let owner = state
            .borrow()
            .nfts
            .get(usize::try_from(token_id)?)
            .ok_or(Error::InvalidTokenId)?
            .owner;
        Ok(owner)
    })
}

#[update(name = "transferFromDip721")]
fn transfer_from(from: Principal, to: Principal, token_id: u64) -> Result {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let state = &mut *state;
        let nft = state
            .nfts
            .get_mut(usize::try_from(token_id)?)
            .ok_or(Error::InvalidTokenId)?;
        let caller = api::caller();
        if nft.owner != caller
            && nft.approved != Some(caller)
            && !state
                .operators
                .get(&from)
                .map(|s| s.contains(&caller))
                .unwrap_or(false)
            && !state.custodians.contains(&caller)
        {
            Err(Error::Unauthorized)
        } else if nft.owner != from {
            Err(Error::Other)
        } else {
            nft.approved = None;
            nft.owner = to;
            Ok(state.next_txid())
        }
    })
}

#[update(name = "safeTransferFromDip721")]
fn safe_transfer_from(from: Principal, to: Principal, token_id: u64) -> Result {
    if to == MGMT {
        Err(Error::ZeroAddress)
    } else {
        transfer_from(from, to, token_id)
    }
}

#[query(name = "supportedInterfacesDip721")]
fn supported_interfaces() -> &'static [InterfaceId] {
    &[
        InterfaceId::TransferNotification,
        InterfaceId::Approval, // Psychedelic/DIP721#5
        InterfaceId::Burn,
        InterfaceId::Mint,
    ]    
}

#[derive(CandidType, Deserialize, Clone)]
struct LogoResult {
    logo_type: Cow<'static, str>,
    data: Cow<'static, str>,
}

#[export_name = "canister_query logoDip721"]
fn logo() /* -> &'static LogoResult */
{
    ic_cdk::setup();
    STATE.with(|state| call::reply((state.borrow().logo.as_ref().unwrap_or(&DEFAULT_LOGO),)))
}

#[query(name = "nameDip721")]
fn name() -> String {
    STATE.with(|state| state.borrow().name.clone())
}

#[query(name = "symbolDip721")]
fn symbol() -> String {
    STATE.with(|state| state.borrow().symbol.clone())
}

const DEFAULT_LOGO: LogoResult = LogoResult {
    data: Cow::Borrowed(include_base64!("logo.png")),
    logo_type: Cow::Borrowed("image/png"),
};

#[query(name = "totalSupplyDip721")]
fn total_supply() -> u64 {
    STATE.with(|state| state.borrow().nfts.len() as u64)
}

#[export_name = "canister_query getMetadataDip721"]
fn get_metadata(/* token_id: u64 */) /* -> Result<&'static MetadataDesc> */
{
    ic_cdk::setup();
    let token_id = call::arg_data::<(u64,)>().0;
    let res: Result<()> = STATE.with(|state| {
        let state = state.borrow();
        let metadata = &state
            .nfts
            .get(usize::try_from(token_id)?)
            .ok_or(Error::InvalidTokenId)?
            .metadata;
        call::reply((Ok::<_, Error>(metadata),));
        Ok(())
    });
    if let Err(e) = res {
        call::reply((Err::<MetadataDesc, _>(e),));
    }
}

#[derive(CandidType)]
struct ExtendedMetadataResult<'a> {
    metadata_desc: MetadataDescRef<'a>,
    token_id: u64,
}

#[export_name = "canister_query getMetadataForUserDip721"]
fn get_metadata_for_user(/* user: Principal */) /* -> Vec<ExtendedMetadataResult> */
{
    ic_cdk::setup();
    let user = call::arg_data::<(Principal,)>().0;
    STATE.with(|state| {
        let state = state.borrow();
        let metadata: Vec<_> = state
            .nfts
            .iter()
            .filter(|n| n.owner == user)
            .map(|n| ExtendedMetadataResult {

                metadata_desc: &n.metadata,
                token_id: n.id,
            })
            .collect();
        call::reply((metadata,));
    });
}

// ----------------------
// notification interface
// ----------------------

#[update(name = "transferFromNotifyDip721")]
fn transfer_from_notify(from: Principal, to: Principal, token_id: u64, data: Vec<u8>) -> Result {
    let res = transfer_from(from, to, token_id)?;
    if let Ok(arg) = Encode!(&api::caller(), &from, &token_id, &data) {
        // Using call_raw ensures we don't need to await the future for the call to be executed.
        // Calling an arbitrary function like this means that a malicious recipient could call 
        // transferFromNotifyDip721 in their onDIP721Received function, resulting in an infinite loop.
        // This will trap eventually, but the transfer will have already been completed and the state-change persisted.
        // That means the original transfer must reply before that happens, or the caller will be
        // convinced that the transfer failed when it actually succeeded. So we don't await the call,
        // so that we'll reply immediately regardless of how long the notification call takes.
        let _ = api::call::call_raw(to, "onDIP721Received", arg, 0);
    }
    Ok(res)
}

#[update(name = "safeTransferFromNotifyDip721")]
fn safe_transfer_from_notify(
    from: Principal,
    to: Principal,
    token_id: u64,
    data: Vec<u8>,
) -> Result {
    if to == MGMT {
        Err(Error::ZeroAddress)
    } else {
        transfer_from_notify(from, to, token_id, data)
    }
}

// ------------------
// approval interface
// ------------------

#[update(name = "approveDip721")]
fn approve(user: Principal, token_id: u64) -> Result {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let state = &mut *state;
        let caller = api::caller();
        let nft = state
            .nfts
            .get_mut(usize::try_from(token_id)?)
            .ok_or(Error::InvalidTokenId)?;
        if nft.owner != caller
            && nft.approved != Some(caller)
            && !state
                .operators
                .get(&user)
                .map(|s| s.contains(&caller))
                .unwrap_or(false)
            && !state.custodians.contains(&caller)
        {
            Err(Error::Unauthorized)
        } else {
            nft.approved = Some(user);
            Ok(state.next_txid())
        }
    })
}

#[update(name = "setApprovalForAllDip721")]
fn set_approval_for_all(operator: Principal, is_approved: bool) -> Result {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let caller = api::caller();
        if operator != caller {
            let operators = state.operators.entry(caller).or_default();
            if operator == MGMT {
                if !is_approved {
                    operators.clear();
                } else {
                    // cannot enable everyone as an operator
                }
            } else {
                if is_approved {
                    operators.insert(operator);
                } else {
                    operators.remove(&operator);
                }
            }
        }
        Ok(state.next_txid())
    })
}

// #[query(name = "getApprovedDip721")] // Psychedelic/DIP721#5
fn _get_approved(token_id: u64) -> Result<Principal> {
    STATE.with(|state| {
        let approved = state
            .borrow()
            .nfts
            .get(usize::try_from(token_id)?)
            .ok_or(Error::InvalidTokenId)?
            .approved
            .unwrap_or_else(api::caller);
        Ok(approved)
    })
}

#[query(name = "isApprovedForAllDip721")]
fn is_approved_for_all(operator: Principal) -> bool {
    STATE.with(|state| {
        state
            .borrow()
            .operators
            .get(&api::caller())
            .map(|s| s.contains(&operator))
            .unwrap_or(false)
    })
}

// --------------
// mint interface
// --------------

#[update(name = "mintDip721")]
fn mint(
    to: Principal,
    metadata: MetadataDesc,
    blob_content: Vec<u8>,
) -> Result<MintResult, ConstrainedError> {
    let (txid, tkid) = STATE.with(|state| {
        let mut state = state.borrow_mut();
        // everyone can mint
        // if !state.custodians.contains(&api::caller()) {
        //     return Err(ConstrainedError::Unauthorized);
        // }
        let new_id = state.nfts.len() as u64;
        let nft = Nft {
            owner: to,
            approved: None,
            id: new_id,
            metadata,
            content: blob_content,
        };
        state.nfts.push(nft);
        Ok((state.next_txid(), new_id))
    })?;
    http::add_hash(tkid);
    Ok(MintResult {
        id: txid,
        token_id: tkid,
    })
}


#[update(name = "simpleMintDip721")]
fn simple_mint(
    to: Principal,
    uri: String,
    mime_type: String,
    name: String,
    origin: String,
) -> Result<MintResult, ConstrainedError> {

    let mut metadata: HashMap<String, MetadataVal> = HashMap::new();
        use MetadataVal::*;
    if uri.len() > 0  {
        if let Err(_) = URI::try_from(&*uri) {
            return Err(ConstrainedError::Unauthorized);
        }
        metadata.insert(String::from("locationType"), Nat8Content(3));
        metadata.insert(String::from("location"), TextContent(uri.clone()));
        metadata.insert(
            String::from("contentHash"),
            BlobContent(Vec::from_iter(Sha256::digest(uri.clone().into_bytes()))),
        );
        metadata.insert(String::from("contentType"), TextContent(mime_type));
        metadata.insert(String::from("name"), TextContent(name));
        metadata.insert(String::from("origin"), TextContent(origin));
    } else {
        return Err(ConstrainedError::Unauthorized);
    }
    let metadata = MetadataPart {
        purpose: MetadataPurpose::Rendered,
        data: vec![],
        key_val_data: metadata,
    };

    STATE.with(|state| {

        let begin_date = &state.borrow().begin_date;
        let end_date = &state.borrow().end_date;

        let now = Utc::now().timestamp_millis();
        let fmt = "%Y-%m-%d %H:%M:%S";
        let begin_date_utc = Utc.datetime_from_str(begin_date, fmt).unwrap().timestamp_millis();
        let end_date_utc = Utc.datetime_from_str(end_date, fmt).unwrap().timestamp_millis();
        
        if now < begin_date_utc || now > end_date_utc {
            return Err(ConstrainedError::TimeError);
        }

        let white_list = &state.borrow().white_list;

        if !white_list.contains(&to){
            return Err(ConstrainedError::Unauthorized);
        }
        mint(to, vec![metadata], vec![])
    })
    
}

#[query(name = "whiteList")]
fn white_list() -> Result<Vec<Principal>> {
    STATE.with(|state| {
        let white_list = state.borrow().white_list.clone();
        ic_cdk::println!("white_list {:?}", white_list);
        Ok(white_list)
    })
}

#[query(name = "nftMintDate")]
fn nft_mint_date() -> String {
    STATE.with(|state| {
        let nft_mint_date = format!("{},{}",state.borrow().begin_date.clone(), state.borrow().end_date.clone());
        ic_cdk::println!("nft_mint_date {:?}", nft_mint_date);
        nft_mint_date
    })
}


#[query(name = "totalLimit")]
fn total_limit() -> String {
    STATE.with(|state| {
        let total_limit = state.borrow().total_limit.clone();
        ic_cdk::println!("total_limit {:?}", total_limit);
        total_limit
    })
}

// --------------
// burn interface
// --------------

#[update(name = "burnDip721")]
fn burn(token_id: u64) -> Result {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let nft = state
            .nfts
            .get_mut(usize::try_from(token_id)?)
            .ok_or(Error::InvalidTokenId)?;
        if nft.owner != api::caller() {
            Err(Error::Unauthorized)
        } else {
            nft.owner = MGMT;
            Ok(state.next_txid())
        }
    })
}

#[derive(CandidType, Deserialize, Default)]
struct State {
    nfts: Vec<Nft>,
    custodians: HashSet<Principal>,
    operators: HashMap<Principal, HashSet<Principal>>, // owner to operators
    logo: Option<LogoResult>,
    name: String,
    symbol: String,
    txid: u128,
    white_list: Vec<Principal>,
    begin_date: String,
    end_date: String,
    total_limit: String,
}

#[derive(CandidType, Deserialize)]
struct Nft {
    owner: Principal,
    approved: Option<Principal>,
    id: u64,
    metadata: MetadataDesc,
    content: Vec<u8>,
}

type MetadataDesc = Vec<MetadataPart>;
type MetadataDescRef<'a> = &'a [MetadataPart];

#[derive(CandidType, Deserialize, Debug)]
struct MetadataPart {
    purpose: MetadataPurpose,
    key_val_data: HashMap<String, MetadataVal>,
    data: Vec<u8>,
}

#[derive(CandidType, Deserialize, PartialEq, Debug)]
enum MetadataPurpose {
    Preview,
    Rendered,
}

#[derive(CandidType, Deserialize)]
struct MintResult {
    token_id: u64,
    id: u128,
}

#[allow(clippy::enum_variant_names)]
#[derive(CandidType, Deserialize, Debug)]
enum MetadataVal {
    TextContent(String),
    BlobContent(Vec<u8>),
    NatContent(u128),
    Nat8Content(u8),
    Nat16Content(u16),
    Nat32Content(u32),
    Nat64Content(u64),
}

impl State {
    fn next_txid(&mut self) -> u128 {
        let txid = self.txid;
        self.txid += 1;
        txid
    }
}

#[derive(CandidType, Deserialize)]
enum InterfaceId {
    Approval,
    TransactionHistory,
    Mint,
    Burn,
    TransferNotification,
}

#[derive(CandidType, Deserialize)]
enum ConstrainedError {
    Unauthorized,
    TimeError,
    // InvalidUri,
}

#[update]
fn set_name(name: String) -> Result<()> {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state.custodians.contains(&api::caller()) {
            state.name = name;
            Ok(())
        } else {
            Err(Error::Unauthorized)
        }
    })
}

#[update]
fn set_symbol(sym: String) -> Result<()> {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state.custodians.contains(&api::caller()) {
            state.symbol = sym;
            Ok(())
        } else {
            Err(Error::Unauthorized)
        }
    })
}

#[update]
fn set_logo(logo: Option<LogoResult>) -> Result<()> {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state.custodians.contains(&api::caller()) {
            state.logo = logo;
            Ok(())
        } else {
            Err(Error::Unauthorized)
        }
    })
}

#[update]
fn set_custodian(user: Principal, custodian: bool) -> Result<()> {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state.custodians.contains(&api::caller()) {
            if custodian {
                state.custodians.insert(user);
            } else {
                state.custodians.remove(&user);
            }
            Ok(())
        } else {
            Err(Error::Unauthorized)
        }
    })
}

#[query]
fn is_custodian(principal: Principal) -> bool {
    STATE.with(|state| state.borrow().custodians.contains(&principal))
}
