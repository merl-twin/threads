use std::{
    thread::{Builder,JoinHandle},
    sync::{
        Arc,
        atomic::{AtomicBool,AtomicUsize,Ordering},
    },
};

use crossbeam::queue::SegQueue;

pub mod monitor {
    mod stat_profiler;
    
    pub use stat_profiler::{
        StatMonitor,
        ThreadInfoIterator, ThreadInfoItem, ThreadInfo,
    };
    
}

lazy_static::lazy_static! {
    pub static ref THREAD_MONITOR: ThreadMonitor = ThreadMonitor::new();
}

pub fn spawn_builder<F, T>(builder: std::thread::Builder, f: F) -> JoinHandle<T>
    where F: FnOnce() -> T, F: Send + 'static, T: Send + 'static
{
    builder.spawn(move || {
        let name = match std::thread::current().name() {
            Some(n) => n.to_string(),
            None => format!("{:?}",std::thread::current().id()),
        };
        let n = THREAD_MONITOR.started(name.clone(),None);
        let r = f();
        THREAD_MONITOR.finished(n,name);
        r
    }).unwrap()
}

pub fn spawn<F, T>(name: String, f: F) -> JoinHandle<T>
    where F: FnOnce() -> T, F: Send + 'static, T: Send + 'static
{
    spawn_builder(Builder::new().name(name),f)
}

pub fn spawn_str<F, T>(name: &str, f: F) -> JoinHandle<T>
    where F: FnOnce() -> T, F: Send + 'static, T: Send + 'static
{
    spawn_builder(Builder::new().name(name.to_string()),f)
}

pub fn spawn_builder_with_state<F, T>(builder: std::thread::Builder,  f: F) -> JoinHandle<T>
    where F: FnOnce(ThreadState) -> T, F: Send + 'static, T: Send + 'static
{
    // like std::thread::spawn, but with name
    builder.spawn(move || {
        let name = match std::thread::current().name() {
            Some(n) => n.to_string(),
            None => format!("{:?}",std::thread::current().id()),
        };
        let state = ThreadState::new();
        let n = THREAD_MONITOR.started(name.clone(),Some(state.clone()));
        let r = f(state.clone());
        state.done();
        THREAD_MONITOR.finished(n,name);
        r
    }).unwrap()
}

pub fn spawn_with_state<F, T>(name: String, f: F) -> JoinHandle<T>
    where F: FnOnce(ThreadState) -> T, F: Send + 'static, T: Send + 'static
{
    spawn_builder_with_state(Builder::new().name(name),f)
}

pub fn spawn_str_with_state<F, T>(name: &str, f: F) -> JoinHandle<T>
    where F: FnOnce(ThreadState) -> T, F: Send + 'static, T: Send + 'static
{
    spawn_builder_with_state(Builder::new().name(name.to_string()),f)
}

#[derive(Debug)]
pub enum ThreadStatus {
    Started{ n: usize, name: String, tm: time::OffsetDateTime, state: Option<ThreadState> },
    Finished{ n: usize, name: String, tm: time::OffsetDateTime },
}

#[derive(Debug,Clone,Copy)]
pub enum State {
    Init,    
    Payload,
    Idle,
    Wait,
    Done,
}
impl State {
    fn opt_from(u: usize) -> Option<State> {
        Some(match u {
            0 => State::Init,
            1 => State::Payload,
            2 => State::Idle,
            3 => State::Wait,
            4 => State::Done,
            _ => return None,
        })
    }
}
impl Into<usize> for State {
    fn into(self) -> usize {
        match self {
            State::Init => 0,
            State::Payload => 1,
            State::Idle => 2,
            State::Wait => 3,
            State::Done => 4,          
        }
    }
}

#[derive(Debug,Clone)]
pub struct ThreadState(Arc<AtomicUsize>);
impl ThreadState {
    fn new() -> ThreadState {
        ThreadState(Arc::new(AtomicUsize::new(State::Init.into())))
    }
    pub fn get(&self) -> Option<State> {
        State::opt_from(self.0.load(Ordering::SeqCst))     
    }
    pub fn payload(&self) {
        self.0.store(State::Payload.into(),Ordering::SeqCst);
    }
    pub fn idle(&self) {
        self.0.store(State::Idle.into(),Ordering::SeqCst);
    }
    pub fn wait(&self) {
        self.0.store(State::Wait.into(),Ordering::SeqCst);
    }
    fn done(&self) {
        self.0.store(State::Done.into(),Ordering::SeqCst);
    }
}

pub struct ThreadMonitor {
    on: AtomicBool,
    next: AtomicUsize,
    messages: SegQueue<ThreadStatus>,
}
impl ThreadMonitor {
    fn new() -> ThreadMonitor {
        log::info!("thread monitor processing is turned OFF");
        ThreadMonitor {
            on: AtomicBool::new(false),
            next: AtomicUsize::new(0),
            messages: SegQueue::new(),
        }
    }
    fn started(&self, name: String, state: Option<ThreadState>) -> usize {
        let n = self.next.fetch_add(1,Ordering::SeqCst);
        log::info!("thread {}: {} has been spawned",n,name);
        if self.on.load(Ordering::SeqCst) {
            self.messages.push(ThreadStatus::Started{ n: n, name, tm: time::OffsetDateTime::now_utc(), state: state });
        }
        n
    }
    fn finished(&self, n: usize, name: String) {
        log::info!("thread {}: {} finished",n,name);
        if self.on.load(Ordering::SeqCst) {
            self.messages.push(ThreadStatus::Finished{ n: n, name, tm: time::OffsetDateTime::now_utc() });
        }        
    }
    pub fn turn_on(&self) {
        log::info!("thread monitor processing is turned ON");
        self.on.store(true,Ordering::SeqCst);
    }
    pub fn turn_off(&self) {
        log::info!("thread monitor processing is turned OFF");
        self.on.store(false,Ordering::SeqCst);
    }
    pub fn process(&self) -> Option<Vec<ThreadStatus>> {
        if self.on.load(Ordering::SeqCst) {
            let mut v = Vec::new();
            while let Some(msg) = self.messages.pop() {
                v.push(msg);
            }
            Some(v)
        } else {
            None
        }
    }
}


