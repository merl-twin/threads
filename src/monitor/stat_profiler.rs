use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize,Ordering},
    },
    time::Instant,
};

use crate::{
    ThreadState, ThreadStatus, State,
};

use oneshot::OneSet;

enum Ask {
    All(OneSet<ThreadInfoIterator>),
}

pub struct ThreadInfoIterator {
    data: ThreadInfoIteratorInner,
}
impl ThreadInfoIterator {
    fn new(data: ThreadInfoIteratorInner) -> ThreadInfoIterator {
        ThreadInfoIterator { data }
    }
}

impl Iterator for ThreadInfoIterator {
    type Item = ThreadInfoItem;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.data {
            ThreadInfoIteratorInner::Static(ar) => {
                for it in ar {
                    let r = it.take();
                    if r.is_some() { return r; }
                }
                None
            },
            ThreadInfoIteratorInner::Vec(v) => v.pop(),
        }
    }
}


#[derive(Debug,Clone)]
pub struct ThreadInfoItem {
    name: Arc<String>,
    info: ThreadInfo,
}
impl ThreadInfoItem {
    pub fn name(&self) -> &str {
        self.name.as_ref()
    }
    pub fn info(&self) -> ThreadInfo {
        self.info
    }
}
    

enum ThreadInfoIteratorInner {
    Static([Option<ThreadInfoItem>; 16]),
    Vec(Vec<ThreadInfoItem>)
}
impl ThreadInfoIteratorInner {
    fn new(sz: usize) -> ThreadInfoIteratorInner {
        if sz > 16 {
            ThreadInfoIteratorInner::Static([None, None, None, None, None, None, None, None, None, None, None, None, None, None, None, None])
        } else {
            ThreadInfoIteratorInner::Vec(Vec::with_capacity(sz))
        }
    }
    fn push(&mut self, name: Arc<String>, info: ThreadInfo) {
        let item = ThreadInfoItem { name, info };
        match self {
            ThreadInfoIteratorInner::Static(ar) => match ar.len() < 16 {
                true => { ar[ar.len()] = Some(item); },
                false => {
                    let mut v = Vec::with_capacity(32);
                    for it in ar {
                        if let Some(it) = it.take() {
                            v.push(it);
                        }
                    }
                    v.push(item);
                    *self = ThreadInfoIteratorInner::Vec(v);
                },
            },
            ThreadInfoIteratorInner::Vec(v) => v.push(item),
        };
    }
}


pub struct StatMonitor {
    asker: Option<crossbeam::channel::Sender<Ask>>,
    monitor: Option<std::thread::JoinHandle<()>>,
    profiler: Option<std::thread::JoinHandle<()>>,
}
impl StatMonitor {
    pub fn new() -> StatMonitor {
        StatMonitor::create(100)
    }

    pub fn get_info(&self) -> Option<ThreadInfoIterator> {
        match &self.asker {
            None => None,
            Some(sender) => {
                let (tx,rx) = oneshot::oneshot();
                match sender.send(Ask::All(tx)) {
                    Err(_) => None,
                    Ok(()) => rx.wait(),
                }
            },
        }
    }
    
    fn create(max_samples_per_sec: u16) -> StatMonitor {
        crate::THREAD_MONITOR.turn_on();
        let (asker,receiver) = crossbeam::channel::unbounded();
        let (tx,rx) = crossbeam::channel::unbounded();
        StatMonitor {
            asker: Some(asker),
            monitor: Some(crate::spawn_str("stat_monitor",move || {              
                let slp = std::time::Duration::new(0,300_000_000);
                #[allow(unused_variables)]
                let mut thr_started = 0;
                #[allow(unused_variables)]
                let mut thr_finished = 0;
                let mut stat_vec = Vec::new();
                loop {
                    match crate::THREAD_MONITOR.process() {
                        None => {
                            log::error!("thread monitor was unexpectedly turned off");
                            break;
                        },
                        Some(v) => for st in v {
                            match st {
                                ThreadStatus::Started{ state: None, .. } => {
                                    thr_started += 1;
                                },
                                ThreadStatus::Started{ n, name, tm, state: Some(state) } => {
                                    thr_started += 1;
                                    let (watcher,agregator) = watcher(state);
                                    stat_vec.push((n,Arc::new(name),tm,agregator));
                                    if let Err(_) = tx.send(watcher) {
                                        log::error!("thread monitor profiler was unexpectedly turned off");
                                        break;
                                    }
                                },
                                ThreadStatus::Finished{ n, .. } => {
                                    thr_finished += 1;
                                    let mut to_remove = None;
                                    for (i,(svn,_,_,_)) in stat_vec.iter().enumerate() {
                                        if *svn == n {
                                            to_remove = Some(i);
                                        }
                                    }
                                    if let Some(idx) = to_remove {
                                        stat_vec.swap_remove(idx);
                                    }
                                },
                            }
                        },
                    }
                    match receiver.try_recv() {
                        Ok(Ask::All(oneset)) => {
                            let mut iter = ThreadInfoIteratorInner::new(stat_vec.len());
                            for (_,name,_,agregator) in &mut stat_vec {
                                iter.push(name.clone(),agregator.get());
                            }
                            oneset.set(ThreadInfoIterator::new(iter));
                        },
                        Err(crossbeam::channel::TryRecvError::Empty) => {},
                        Err(crossbeam::channel::TryRecvError::Disconnected) => {
                            log::error!("stat_monitor was unexpectedly turned off");
                            break;
                        },
                    }
                    
                    std::thread::sleep(slp);
                }
            })),
            profiler: Some(crate::spawn_str("stat_profiler",move || {
                let sps = 1u32 + max_samples_per_sec as u32;
                let slp = std::time::Duration::new(0,1_000_000_000 / sps);
                let mut to_watch = Vec::with_capacity(1024);
                loop {
                    match rx.try_recv() {
                        Ok(watcher) => {
                            to_watch.push(watcher);
                            if sps <= 10 {
                                while let Ok(watcher) = rx.try_recv() {
                                    to_watch.push(watcher);
                                }
                            }
                        },
                        Err(crossbeam::channel::TryRecvError::Empty) => {},
                        Err(crossbeam::channel::TryRecvError::Disconnected) => {
                            log::error!("thread monitor main was unexpectedly turned off");
                            break;
                        },
                    }

                    let mut to_remove = None;
                    for (i,w) in to_watch.iter().enumerate() {
                        if !w.process() {
                            to_remove = Some(i);
                        }
                    }
                    if let Some(idx) = to_remove {
                        // removing rate may be too slow if:
                        //    1) sps is lower then 10
                        //    2) spawn rate is higher then sps 
                        to_watch.swap_remove(idx);
                    }

                    std::thread::sleep(slp);
                }
            })),
        }
    }
}
impl Drop for StatMonitor {
    fn drop(&mut self) {
        std::mem::drop(self.asker.take());
        if let Some(h) = self.monitor.take() {
            if let Err(e) = h.join() {
                log::warn!("error joining monitor thread: {:?}",e);
            }
        }
        if let Some(h) = self.profiler.take() {
            if let Err(e) = h.join() {
                log::warn!("error joining profiler thread: {:?}",e);
            }
        }
    }
}

#[derive(Debug,Clone,Copy)]
pub struct ThreadInfo {
    pub payload: usize,
    pub idle: usize,
    pub wait: usize,
    pub measures: usize,
    pub interval: f64,
}

fn watcher(state: ThreadState) -> (StateWatcher,ThreadStatistics) {
    let payload = Arc::new(AtomicUsize::new(0));
    let idle = Arc::new(AtomicUsize::new(0));
    let wait = Arc::new(AtomicUsize::new(0));
    let unknown = Arc::new(AtomicUsize::new(0));
    let measures = Arc::new(AtomicUsize::new(0));
    let sw = StateWatcher{
        state: state,
        payload: payload.clone(),
        idle: idle.clone(),
        wait: wait.clone(),
        unknown: unknown.clone(),
        measures: measures.clone(),
    };
    let ts = ThreadStatistics {
        payload: payload,
        idle: idle,
        wait: wait,
        unknown: unknown,
        measures: measures,

        prev_payload: 0,
        prev_idle: 0,
        prev_wait: 0,
        prev_unknown: 0,
        prev_measures: 0,
        prev_tm: Instant::now(),
    };
    (sw,ts)
}

struct ThreadStatistics {
    payload: Arc<AtomicUsize>,
    idle: Arc<AtomicUsize>,
    wait: Arc<AtomicUsize>,
    unknown: Arc<AtomicUsize>,
    measures: Arc<AtomicUsize>,

    prev_payload: usize,
    prev_idle: usize,
    prev_wait: usize,
    prev_unknown: usize,
    prev_measures: usize,
    prev_tm: Instant,
}

impl ThreadStatistics {
    fn get(&mut self) -> ThreadInfo {
        let unk = self.unknown.load(Ordering::Relaxed);
        let pld = self.payload.load(Ordering::Relaxed);
        let idle = self.idle.load(Ordering::Relaxed);
        let wait = self.wait.load(Ordering::Relaxed);
        let ms = self.measures.load(Ordering::Relaxed);
        let _u = unk.wrapping_sub(self.prev_unknown); self.prev_unknown = unk;
        let p = pld.wrapping_sub(self.prev_payload);  self.prev_payload = pld;
        let i = idle.wrapping_sub(self.prev_idle);    self.prev_idle = idle;
        let w = wait.wrapping_sub(self.prev_wait);    self.prev_wait = wait;
        let m = ms.wrapping_sub(self.prev_measures);  self.prev_measures = ms;
        let t = self.prev_tm.elapsed().as_secs_f64(); self.prev_tm = Instant::now();
        ThreadInfo {
            payload: p,
            idle: i,
            wait: w,
            measures: m,
            interval: t,
        }
    }
}



struct StateWatcher {
    state: ThreadState,
    payload: Arc<AtomicUsize>,
    idle: Arc<AtomicUsize>,
    wait: Arc<AtomicUsize>,
    unknown: Arc<AtomicUsize>,
    measures: Arc<AtomicUsize>,
}

impl StateWatcher {
    fn process(&self) -> bool {
        match self.state.get() {
            Some(s) => {    
                match s {
                    State::Init => { self.unknown.fetch_add(1,Ordering::Relaxed); },    
                    State::Payload => { self.payload.fetch_add(1,Ordering::Relaxed); },
                    State::Idle => { self.idle.fetch_add(1,Ordering::Relaxed); },
                    State::Wait => { self.wait.fetch_add(1,Ordering::Relaxed); },
                    State::Done => return false,
                }
                self.measures.fetch_add(1,Ordering::Relaxed);
                true
            },
            None => false,
        }
    }
}
