#![feature(proc_macro_hygiene, decl_macro)]
#[macro_use]
extern crate rocket;
#[macro_use]
extern crate lazy_static;

use rand::Rng;
use parking_lot::RwLock;
use std::collections::*;
use std::sync::atomic::Ordering;

#[derive(Debug)]
pub struct RemoveInfo {
	pub at: std::time::Instant,
	pub link: String,
	pub code: String,
}

lazy_static! {
	// 순서에 따라서 push 됨. 그렇기에 맨 앞은 가장 오래된것. 맨 뒤는 가장 최근것. iter 하면서 삭제 처리하면 됨.
	pub static ref G_REMOVE_QUEUE: RwLock<Vec<RemoveInfo>> =
		RwLock::new(Vec::with_capacity(1024));
	pub static ref G_CODE_TO_LINK: RwLock<HashMap<String, String>> =
		RwLock::new(HashMap::with_capacity(1024));
	pub static ref G_LINK_TO_CODE: RwLock<HashMap<String, String>> =
		RwLock::new(HashMap::with_capacity(1024));

	// load() 할 때마다 삭제를 하기 위해서 G_REMOVE_QUEUE 를 write Lock 걸면 성능이 아깝.
	// 이게 일종의 세마포어 역할을 하면서 cnt가 0일때만 remove를 시도함. (create()로 인한 G_REMOVE_QUEUE 락을 피할려는 목적)
	pub static ref G_CREATE_CNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
}

#[inline(always)]
fn instant_to_remove() -> std::time::Instant {
	return instant_now() + std::time::Duration::from_secs(3 * 60 * 60);
}
#[inline(always)]
fn instant_now() -> std::time::Instant {
	return std::time::Instant::now();
}

#[get("/create/<url>")]
fn create(mut url: String) -> Option<String> {
	// 최소 글자는 5글자. 현실적으로 가장 짧을만한 주소가 뭔지 생각해봤는데 op.gg(5글자)가 현실적인 선에서 타당할 것 같음.
	if url.chars().count() < 5 {
		return None;
	}

	if url.contains("://") == false {
		url = format!("http://{}", url);
	}
	
	return create_inner(url, None);
}

//Recursive한 상황에서 Lock을 넘겨주기 위해서 한번 더 감쌈.
fn create_inner(url: String, lock: Option<parking_lot::RwLockWriteGuard<std::vec::Vec<RemoveInfo>>>) -> Option<String> {
// 이걸 미리 락 안하면 잘못하다가 다른 쓰레드로 인해 중간에 날라갈 수도 있음.
	// G_LINK_TO_CODE 조회때는 있는데, 그 사이에 다른 쓰레드에서 G_REMOVE_QUEUE 에 따라서 삭제 시도하면 중간에 펑! 해버리는것
	G_CREATE_CNT.fetch_add(1, Ordering::SeqCst);

	let mut LOCK_REM_QUEUE = match lock {
		None => G_REMOVE_QUEUE.write(),
		Some(l) => l
	};

	let if_exist_with_code = {
		let table = G_LINK_TO_CODE.read();

		table.get(&url).map(|code| code.clone())
	};

	let result = match if_exist_with_code {
		Some(code) => {
			let pos = LOCK_REM_QUEUE.iter().position(|item| item.code == code);
			match pos {
				None => {
					// 위에서 Lock이 걸린상태여서... None이 나올 리는 없긴한데... 혹시 몰라서..
					// None이 나오면 LINK_TO_CODE하고 REMOVE_QUEUE하고 동기화가 안되었다는것.
					// LINK_TO_CODE 와 CODE_TO_LINK 에서 항목을 제거함. 그리고 새로 시도.

					G_LINK_TO_CODE.write().remove(&url);
					G_CODE_TO_LINK.write().remove(&code);

					//주의 recursive할 수 있음! write-lock을 
					return create_inner(url, Some(LOCK_REM_QUEUE));
				},
				Some(p) => {
					let mut rinfo = LOCK_REM_QUEUE.remove(p);
					rinfo.at = instant_to_remove();
					LOCK_REM_QUEUE.push(rinfo);

					Some(code)
				}
			}
		}
		None => {
			let mut rnd = rand::thread_rng();

			let code = 
				loop {
					let rnd = rnd.gen_range(0, 999999).to_string();
					if G_CODE_TO_LINK.read().contains_key(&rnd) == false {
						break rnd;
					}
				};

			let rinfo = RemoveInfo {
				at: instant_to_remove(),
				link: url.clone(),
				code: code.clone()
			};

			{ G_CODE_TO_LINK.write().insert(code.clone(), url.clone()); }
			{ G_LINK_TO_CODE.write().insert(url, code.clone()); }
			{ LOCK_REM_QUEUE.push(rinfo); }

			Some(code)
		}
	};

	G_CREATE_CNT.fetch_sub(1, Ordering::SeqCst);
	return result;
}

#[get("/code/<code>")]
fn load(code: u8) -> Option<String> {
	let code = code.to_string();
	let table = G_CODE_TO_LINK.read();

	return match table.get(&code) {
		None => None,
		Some(s) => Some(s.clone()),
	};
}

#[get("/info-esm-status")]
fn info() -> String {
	return format!("{:?}\n<br>{:?}\n<br>{:?}", G_REMOVE_QUEUE.read(), G_CODE_TO_LINK.read(), G_LINK_TO_CODE.read());
}

/// CREATE_CNT를 검증하는건 여기에 포함되지 않음! 구현하는 측에서 꼭 체크할 것.
fn remove_expired() {
	// 삭제 할 게 없으면 굳이 write lock까지 걸 필요는 없음. Read선에서 해결
	let expired_exist = {
		let rque = G_REMOVE_QUEUE.read();
		if rque.len() == 0 { return; }

		if rque[0].at > instant_now() { return; }

		true
	};

	// 위에서 false 될 경우에 미리 return을 했기에 따로 처리 없는것. 
	// return 없이 넘어가는 경우가 있다면 추가 코드 작성 해야함.

	let mut rque = G_REMOVE_QUEUE.write();
	while rque.len() > 0 {
		if rque[0].at > instant_now() { break; }

		rque.remove(0);
	}
}

fn main() {
	use rocket::config::{Config, Environment};

	let config = Config::build(Environment::Staging)
		.address("127.0.0.1")
		.port(8083)
		.finalize()
		.unwrap();

	rocket::custom(config)
		.mount("/backend", routes![create, load, info])
		.launch();
}
