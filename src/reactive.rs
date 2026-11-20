use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::ops::Deref;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_FLUSH_COMPUTATIONS: usize = 4096;
static NEXT_COMPUTATION: AtomicU64 = AtomicU64::new(1);

type Callback = Box<dyn FnMut(&mut Context)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReactiveError {
    DisposedOwner,
    ReentrantFlush,
    RunawayUpdates,
}

impl fmt::Display for ReactiveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::DisposedOwner => "reactive owner is disposed",
            Self::ReentrantFlush => "reactive flush cannot run inside another flush",
            Self::RunawayUpdates => "reactive flush exceeded 4096 computations",
        })
    }
}

impl Error for ReactiveError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Memo,
    Effect,
}

trait Dependency {
    fn subscribe(&self, computation: &Rc<Computation>);
    fn unsubscribe(&self, id: u64);
}

struct Source<T> {
    value: RefCell<T>,
    subscribers: RefCell<BTreeMap<u64, Weak<Computation>>>,
}

impl<T> Dependency for Source<T> {
    fn subscribe(&self, computation: &Rc<Computation>) {
        self.subscribers
            .borrow_mut()
            .insert(computation.id, Rc::downgrade(computation));
    }

    fn unsubscribe(&self, id: u64) {
        self.subscribers.borrow_mut().remove(&id);
    }
}

#[derive(Clone)]
pub struct ReadSignal<T> {
    source: Rc<Source<T>>,
}

impl<T: Clone> ReadSignal<T> {
    pub fn get_untracked(&self) -> T {
        self.source.value.borrow().clone()
    }
}

#[derive(Clone)]
pub struct Signal<T>(ReadSignal<T>);

impl<T> Deref for Signal<T> {
    type Target = ReadSignal<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: PartialEq> Signal<T> {
    pub fn set(&self, value: T) -> bool {
        {
            let mut current = self.source.value.borrow_mut();
            if *current == value {
                return false;
            }
            *current = value;
        }
        let subscribers: Vec<_> = self
            .source
            .subscribers
            .borrow()
            .values()
            .filter_map(Weak::upgrade)
            .collect();
        for computation in subscribers {
            computation.schedule();
        }
        true
    }
}

#[derive(Clone)]
pub struct Memo<T>(ReadSignal<T>);

impl<T> Deref for Memo<T> {
    type Target = ReadSignal<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub struct Bound<T> {
    value: ReadSignal<T>,
    dirty: Rc<Cell<bool>>,
}

impl<T: Clone> Bound<T> {
    pub fn get(&self) -> T {
        self.value.get_untracked()
    }

    pub fn take_damage(&self) -> bool {
        self.dirty.replace(false)
    }
}

pub struct Context {
    computation: Rc<Computation>,
    dependencies: Vec<Weak<dyn Dependency>>,
}

impl Context {
    pub fn read<T: Clone + 'static>(&mut self, value: &ReadSignal<T>) -> T {
        let dependency: Rc<dyn Dependency> = value.source.clone();
        let weak = Rc::downgrade(&dependency);
        if !self
            .dependencies
            .iter()
            .any(|existing| existing.ptr_eq(&weak))
        {
            dependency.subscribe(&self.computation);
            self.dependencies.push(weak);
        }
        value.get_untracked()
    }
}

struct Computation {
    id: u64,
    kind: Kind,
    runtime: Weak<RefCell<State>>,
    callback: RefCell<Option<Callback>>,
    dependencies: RefCell<Vec<Weak<dyn Dependency>>>,
}

impl Computation {
    fn schedule(&self) {
        if let Some(runtime) = self.runtime.upgrade() {
            let mut state = runtime.borrow_mut();
            if state.computations.contains_key(&self.id) {
                match self.kind {
                    Kind::Memo => state.memos.insert(self.id),
                    Kind::Effect => state.effects.insert(self.id),
                };
            }
        }
    }

    fn unsubscribe(&self) {
        let dependencies = std::mem::take(&mut *self.dependencies.borrow_mut());
        for dependency in dependencies.into_iter().filter_map(|weak| weak.upgrade()) {
            dependency.unsubscribe(self.id);
        }
    }

    fn dispose(&self) {
        self.unsubscribe();
        let callback = self.callback.borrow_mut().take();
        drop(callback);
    }
}

impl Drop for Computation {
    fn drop(&mut self) {
        self.unsubscribe();
    }
}

#[derive(Default)]
struct State {
    computations: BTreeMap<u64, Rc<Computation>>,
    memos: BTreeSet<u64>,
    effects: BTreeSet<u64>,
    flushing: bool,
}

#[derive(Clone, Default)]
pub struct Runtime {
    state: Rc<RefCell<State>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlushStats {
    pub memos: usize,
    pub effects: usize,
    pub pending: bool,
}

struct FlushGuard(Rc<RefCell<State>>);

impl Drop for FlushGuard {
    fn drop(&mut self) {
        self.0.borrow_mut().flushing = false;
    }
}

impl Runtime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn signal<T>(&self, value: T) -> Signal<T> {
        Signal(ReadSignal {
            source: Rc::new(Source {
                value: RefCell::new(value),
                subscribers: RefCell::new(BTreeMap::new()),
            }),
        })
    }

    pub fn owner(&self) -> Owner {
        Owner(Rc::new(Ownership {
            runtime: self.clone(),
            computations: RefCell::new(Vec::new()),
            children: RefCell::new(Vec::new()),
            disposed: Cell::new(false),
        }))
    }

    pub fn needs_flush(&self) -> bool {
        let state = self.state.borrow();
        !state.memos.is_empty() || !state.effects.is_empty()
    }

    pub fn flush(&self) -> Result<FlushStats, ReactiveError> {
        {
            let mut state = self.state.borrow_mut();
            if state.flushing {
                return Err(ReactiveError::ReentrantFlush);
            }
            state.flushing = true;
        }
        let _guard = FlushGuard(self.state.clone());
        let mut stats = FlushStats::default();
        let mut ran_effects = BTreeSet::new();
        loop {
            let next = {
                let state = self.state.borrow();
                state
                    .memos
                    .first()
                    .copied()
                    .map(|id| (id, Kind::Memo))
                    .or_else(|| {
                        state
                            .effects
                            .iter()
                            .find(|id| !ran_effects.contains(*id))
                            .copied()
                            .map(|id| (id, Kind::Effect))
                    })
            };
            let Some((id, kind)) = next else { break };
            if stats.memos + stats.effects == MAX_FLUSH_COMPUTATIONS {
                return Err(ReactiveError::RunawayUpdates);
            }
            {
                let mut state = self.state.borrow_mut();
                match kind {
                    Kind::Memo => {
                        state.memos.remove(&id);
                        stats.memos += 1;
                    }
                    Kind::Effect => {
                        state.effects.remove(&id);
                        ran_effects.insert(id);
                        stats.effects += 1;
                    }
                }
            }
            self.run(id);
        }
        stats.pending = self.needs_flush();
        Ok(stats)
    }

    fn run(&self, id: u64) {
        let computation = self.state.borrow().computations.get(&id).cloned();
        let Some(computation) = computation else {
            return;
        };
        let callback = computation.callback.borrow_mut().take();
        let Some(mut callback) = callback else { return };
        computation.unsubscribe();
        let mut context = Context {
            computation: computation.clone(),
            dependencies: Vec::new(),
        };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            callback(&mut context);
        }));
        if let Err(panic) = outcome {
            // A failed computation cannot safely resume with partially updated captures.
            let mut state = self.state.borrow_mut();
            state.computations.remove(&id);
            state.memos.remove(&id);
            state.effects.remove(&id);
            drop(state);
            for dependency in context
                .dependencies
                .into_iter()
                .filter_map(|weak| weak.upgrade())
            {
                dependency.unsubscribe(id);
            }
            computation.dispose();
            std::panic::resume_unwind(panic);
        }
        if self.state.borrow().computations.contains_key(&id) {
            *computation.dependencies.borrow_mut() = context.dependencies;
            *computation.callback.borrow_mut() = Some(callback);
        } else {
            for dependency in context
                .dependencies
                .into_iter()
                .filter_map(|weak| weak.upgrade())
            {
                dependency.unsubscribe(id);
            }
        }
    }
}

struct Ownership {
    runtime: Runtime,
    computations: RefCell<Vec<u64>>,
    children: RefCell<Vec<Weak<Ownership>>>,
    disposed: Cell<bool>,
}

impl Ownership {
    fn dispose(&self) {
        if self.disposed.replace(true) {
            return;
        }
        let children = std::mem::take(&mut *self.children.borrow_mut());
        for child in children.into_iter().filter_map(|weak| weak.upgrade()) {
            child.dispose();
        }
        let ids = std::mem::take(&mut *self.computations.borrow_mut());
        for id in ids {
            let computation = {
                let mut state = self.runtime.state.borrow_mut();
                state.memos.remove(&id);
                state.effects.remove(&id);
                state.computations.remove(&id)
            };
            if let Some(computation) = computation {
                computation.dispose();
            }
        }
    }
}

pub struct Owner(Rc<Ownership>);

impl Owner {
    pub fn is_disposed(&self) -> bool {
        self.0.disposed.get()
    }

    pub fn child(&self) -> Result<Self, ReactiveError> {
        if self.is_disposed() {
            return Err(ReactiveError::DisposedOwner);
        }
        let child = self.0.runtime.owner();
        let mut children = self.0.children.borrow_mut();
        children.retain(|child| child.strong_count() > 0);
        children.push(Rc::downgrade(&child.0));
        Ok(child)
    }

    pub fn effect(
        &self,
        callback: impl FnMut(&mut Context) + 'static,
    ) -> Result<(), ReactiveError> {
        self.add(Kind::Effect, Box::new(callback))
    }

    pub fn memo<T: Clone + PartialEq + 'static>(
        &self,
        initial: T,
        mut compute: impl FnMut(&mut Context) -> T + 'static,
    ) -> Result<Memo<T>, ReactiveError> {
        let value = self.0.runtime.signal(initial);
        let result = Memo(value.0.clone());
        self.add(
            Kind::Memo,
            Box::new(move |context| {
                value.set(compute(context));
            }),
        )?;
        Ok(result)
    }

    pub fn bind<T: Clone + PartialEq + 'static>(
        &self,
        initial: T,
        mut compute: impl FnMut(&mut Context) -> T + 'static,
    ) -> Result<Bound<T>, ReactiveError> {
        let value = self.0.runtime.signal(initial);
        let dirty = Rc::new(Cell::new(true));
        let result = Bound {
            value: value.0.clone(),
            dirty: dirty.clone(),
        };
        self.effect(move |context| {
            if value.set(compute(context)) {
                dirty.set(true);
            }
        })?;
        Ok(result)
    }

    fn add(&self, kind: Kind, callback: Callback) -> Result<(), ReactiveError> {
        if self.is_disposed() {
            return Err(ReactiveError::DisposedOwner);
        }
        let id = NEXT_COMPUTATION.fetch_add(1, Ordering::Relaxed);
        let computation = Rc::new(Computation {
            id,
            kind,
            runtime: Rc::downgrade(&self.0.runtime.state),
            callback: RefCell::new(Some(callback)),
            dependencies: RefCell::new(Vec::new()),
        });
        self.0
            .runtime
            .state
            .borrow_mut()
            .computations
            .insert(id, computation.clone());
        self.0.computations.borrow_mut().push(id);
        computation.schedule();
        Ok(())
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.0.dispose();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_sets_coalesce_to_one_effect_per_flush() -> Result<(), ReactiveError> {
        let runtime = Runtime::new();
        let owner = runtime.owner();
        let signal = runtime.signal(0);
        let observed = Rc::new(RefCell::new(Vec::new()));
        let input = signal.clone();
        let output = observed.clone();
        owner.effect(move |context| output.borrow_mut().push(context.read(&input)))?;
        assert_eq!(runtime.flush()?.effects, 1);
        signal.set(1);
        signal.set(2);
        signal.set(3);
        assert_eq!(runtime.flush()?.effects, 1);
        assert_eq!(*observed.borrow(), [0, 3]);
        assert!(!signal.set(3));
        assert_eq!(runtime.flush()?.effects, 0);
        Ok(())
    }

    #[test]
    fn changing_branches_unsubscribes_stale_dependencies() -> Result<(), ReactiveError> {
        let runtime = Runtime::new();
        let owner = runtime.owner();
        let choose_left = runtime.signal(true);
        let left = runtime.signal(1);
        let right = runtime.signal(2);
        let branch = choose_left.clone();
        let lhs = left.clone();
        let rhs = right.clone();
        let value = owner.bind(0, move |context| {
            if context.read(&branch) {
                context.read(&lhs)
            } else {
                context.read(&rhs)
            }
        })?;
        runtime.flush()?;
        assert_eq!(value.get(), 1);
        choose_left.set(false);
        runtime.flush()?;
        assert_eq!(value.get(), 2);
        left.set(9);
        assert!(!runtime.needs_flush());
        right.set(7);
        runtime.flush()?;
        assert_eq!(value.get(), 7);
        Ok(())
    }

    #[test]
    fn unchanged_memo_does_not_notify_its_consumers() -> Result<(), ReactiveError> {
        let runtime = Runtime::new();
        let owner = runtime.owner();
        let input = runtime.signal(0);
        let source = input.clone();
        let parity = owner.memo(0, move |context| context.read(&source) % 2)?;
        let view = owner.bind(0, move |context| context.read(&parity))?;
        runtime.flush()?;
        view.take_damage();
        input.set(2);
        assert_eq!(
            runtime.flush()?,
            FlushStats {
                memos: 1,
                effects: 0,
                pending: false
            }
        );
        assert!(!view.take_damage());
        input.set(3);
        assert_eq!(runtime.flush()?.effects, 1);
        assert_eq!(view.get(), 1);
        assert!(view.take_damage());
        Ok(())
    }

    #[test]
    fn disposing_owner_releases_callbacks_and_disables_effects() -> Result<(), ReactiveError> {
        let runtime = Runtime::new();
        let signal = runtime.signal(0);
        let owner = runtime.owner();
        let marker = Rc::new(Cell::new(0));
        let weak = Rc::downgrade(&marker);
        let source = signal.clone();
        owner.effect(move |context| marker.set(context.read(&source)))?;
        runtime.flush()?;
        assert!(weak.upgrade().is_some());
        drop(owner);
        assert!(weak.upgrade().is_none());
        signal.set(1);
        assert!(!runtime.needs_flush());
        assert_eq!(runtime.flush()?.effects, 0);
        Ok(())
    }

    #[test]
    fn parent_disposal_disables_child_owners() -> Result<(), ReactiveError> {
        let runtime = Runtime::new();
        let parent = runtime.owner();
        let child = parent.child()?;
        child.effect(|_| {})?;
        drop(parent);
        assert!(child.is_disposed());
        assert_eq!(child.effect(|_| {}), Err(ReactiveError::DisposedOwner));
        assert_eq!(runtime.flush()?.effects, 0);
        Ok(())
    }

    #[test]
    fn child_churn_keeps_ownership_registry_bounded() -> Result<(), ReactiveError> {
        let runtime = Runtime::new();
        let parent = runtime.owner();
        let retained = parent.child()?;
        for _ in 0..1000 {
            drop(parent.child()?);
            assert_eq!(parent.0.children.borrow().len(), 2);
        }
        drop(parent);
        assert!(retained.is_disposed());
        Ok(())
    }

    #[test]
    fn state_changes_dirty_bound_widget_without_manual_invalidation() -> Result<(), ReactiveError> {
        let runtime = Runtime::new();
        let owner = runtime.owner();
        let count = runtime.signal(0);
        let source = count.clone();
        let widget = owner.bind(String::new(), move |context| {
            format!("{} workspaces", context.read(&source))
        })?;
        runtime.flush()?;
        assert!(widget.take_damage());
        assert!(!widget.take_damage());
        count.set(3);
        runtime.flush()?;
        assert_eq!(widget.get(), "3 workspaces");
        assert!(widget.take_damage());
        assert!(!widget.take_damage());
        Ok(())
    }

    #[test]
    fn memo_chains_settle_before_widget_effects() -> Result<(), ReactiveError> {
        let runtime = Runtime::new();
        let owner = runtime.owner();
        let value = runtime.signal(0);
        let source = value.clone();
        let doubled = owner.memo(0, move |context| context.read(&source) * 2)?;
        let incremented = owner.memo(0, move |context| context.read(&doubled) + 1)?;
        let widget = owner.bind(0, move |context| context.read(&incremented) * 3)?;
        runtime.flush()?;
        assert_eq!(widget.get(), 3);
        value.set(2);
        assert_eq!(
            runtime.flush()?,
            FlushStats {
                memos: 2,
                effects: 1,
                pending: false
            }
        );
        assert_eq!(widget.get(), 15);
        Ok(())
    }

    #[test]
    fn self_updates_wait_for_the_next_flush() -> Result<(), ReactiveError> {
        let runtime = Runtime::new();
        let owner = runtime.owner();
        let value = runtime.signal(0);
        let source = value.clone();
        owner.effect(move |context| {
            let current = context.read(&source);
            if current < 3 {
                source.set(current + 1);
            }
        })?;
        for expected in 1..=3 {
            assert_eq!(runtime.flush()?.effects, 1);
            assert_eq!(value.get_untracked(), expected);
            assert!(runtime.needs_flush());
        }
        assert_eq!(
            runtime.flush()?,
            FlushStats {
                memos: 0,
                effects: 1,
                pending: false
            }
        );
        Ok(())
    }

    #[test]
    fn runaway_memo_updates_return_an_error_instead_of_hanging() -> Result<(), ReactiveError> {
        let runtime = Runtime::new();
        let owner = runtime.owner();
        let source = runtime.signal(0);
        let _memo = owner.memo(0, move |context| {
            let previous = context.read(&source);
            source.set(previous + 1);
            previous
        })?;
        assert_eq!(runtime.flush(), Err(ReactiveError::RunawayUpdates));
        drop(owner);
        assert!(!runtime.needs_flush());
        assert_eq!(runtime.flush()?, FlushStats::default());
        Ok(())
    }

    #[test]
    fn disposal_during_a_callback_removes_new_dependencies() -> Result<(), ReactiveError> {
        let runtime = Runtime::new();
        let source = runtime.signal(1);
        let owner = Rc::new(RefCell::new(Some(runtime.owner())));
        let marker = Rc::new(Cell::new(0));
        let weak = Rc::downgrade(&marker);
        let slot = owner.clone();
        let input = source.clone();
        owner
            .borrow()
            .as_ref()
            .expect("owner exists")
            .effect(move |context| {
                marker.set(context.read(&input));
                drop(slot.borrow_mut().take());
            })?;
        runtime.flush()?;
        assert!(weak.upgrade().is_none());
        assert!(source.source.subscribers.borrow().is_empty());
        source.set(2);
        assert!(!runtime.needs_flush());
        Ok(())
    }

    #[test]
    fn reentrant_flush_is_rejected_without_poisoning_the_runtime() -> Result<(), ReactiveError> {
        let runtime = Runtime::new();
        let owner = runtime.owner();
        let nested = runtime.clone();
        let result = Rc::new(RefCell::new(None));
        let output = result.clone();
        owner.effect(move |_| *output.borrow_mut() = Some(nested.flush()))?;
        runtime.flush()?;
        assert_eq!(*result.borrow(), Some(Err(ReactiveError::ReentrantFlush)));
        assert_eq!(runtime.flush()?, FlushStats::default());
        Ok(())
    }

    #[test]
    fn panicking_callback_is_disposed_before_unwinding() -> Result<(), ReactiveError> {
        let runtime = Runtime::new();
        let owner = runtime.owner();
        let source = runtime.signal(0);
        let input = source.clone();
        let marker = Rc::new(());
        let weak = Rc::downgrade(&marker);
        owner.effect(move |context| {
            let _keep = &marker;
            context.read(&input);
            input.set(1);
            panic!("failed computation");
        })?;
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runtime.flush())).is_err()
        );
        assert!(weak.upgrade().is_none());
        assert!(source.source.subscribers.borrow().is_empty());
        source.set(2);
        assert!(!runtime.needs_flush());
        assert_eq!(runtime.flush()?, FlushStats::default());
        let observed = Rc::new(Cell::new(0));
        let output = observed.clone();
        owner.effect(move |context| output.set(context.read(&source)))?;
        runtime.flush()?;
        assert_eq!(observed.get(), 2);
        Ok(())
    }
}
