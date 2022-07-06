use crate::parser::{Event, EventKind};
use crate::schema::{Item, Location, MatchResult, Primary, Schema, Syntax};
use crate::{Error, Result};
use std::fmt::{Debug, Display};
use std::hash::Hash;

#[derive(Clone, Debug)]
pub struct Path<'s, ID, E: Item>
where
  ID: Clone + Display + Debug,
{
  schema: &'s Schema<ID, E>,
  event_buffer: Vec<Event<ID, E>>,
  stack: Vec<StackFrame<'s, ID, E>>,
}

impl<'s, ID, E: Item> Path<'s, ID, E>
where
  ID: Clone + Hash + Ord + Display + Debug,
{
  pub fn new(id: &ID, schema: &'s Schema<ID, E>) -> Result<E, Self> {
    let event_buffer = Vec::with_capacity(16);
    let stack = Vec::with_capacity(16);

    let mut path = Self { schema, event_buffer, stack };
    path.stack_push_alias(id)?;
    Ok(path)
  }

  pub fn current(&self) -> &State<'s, ID, E> {
    &self.stack.last().unwrap().state
  }

  pub fn current_mut(&mut self) -> &mut State<'s, ID, E> {
    &mut self.stack.last_mut().unwrap().state
  }

  /// return false if the end of reached.
  /// returns (matched, confirmed), where matched=true, it needs to move to term and continue
  /// processing, and confirmed=true
  /// Note that if called by matched=false, it may be overriden by matched=true at the upper layer
  /// of the stack.
  ///
  pub fn move_to_next(&mut self, buffer: &[E], mut matched: bool, eof: bool) -> (bool, bool) {
    for i in 0..self.stack.len() {
      let stack_position = self.stack.len() - i - 1;
      let StackFrame { state, current, parent } = &mut self.stack[stack_position];
      debug_assert!(state.appearances <= *state.syntax().repetition.end());

      if matched {
        state.appearances += 1;
      }

      matched = match (matched, eof) {
        (true, true) => state.appearances >= *state.syntax().repetition.start(),
        (true, false) => {
          if state.appearances < *state.syntax().repetition.end() {
            println!("~ repeated: {} / {}", state.syntax(), state.appearances);
            state.proceed_along_buffer(buffer);
            self.stack_pop(i);
            return (true, false);
          }
          debug_assert!(state.appearances == *state.syntax().repetition.end());
          true
        }
        (false, _) => state.appearances >= *state.syntax.repetition.start(),
      };

      if matched {
        state.proceed_along_buffer(buffer);
        if *current + 1 < parent.len() {
          println!("~ moved: {} -> {}", parent[*current], parent[*current + 1]);
          state.appearances = 0;
          state.syntax = &parent[*current + 1];
          *current += 1;
          self.stack_pop(i);
          return (true, false);
        }
      }
    }

    println!("~ confirmed: {} ({})", self.current().syntax(), if matched { "Matched" } else { "Unmatched" });
    (matched, true)
  }

  pub fn completed(&mut self) {
    self.stack_pop(self.stack.len() - 1);
    debug_assert!(self.stack.len() == 1);
    debug_assert!(self.stack[0].current + 1 == self.stack[0].parent.len());
  }

  pub fn stack_push_alias(&mut self, id: &ID) -> Result<E, ()> {
    println!("~ begined: {}", id);
    self.stack_push(Self::get_definition(id, self.schema)?);
    Ok(())
  }

  pub fn stack_push(&mut self, seq: &'s Vec<Syntax<ID, E>>) {
    let mut sf = StackFrame::new(seq);
    if !self.stack.is_empty() {
      sf.state.location = self.current().location;
      sf.state.match_begin = self.current().match_begin;
    }
    self.stack.push(sf);
  }

  fn stack_pop(&mut self, count: usize) {
    for _ in 0..count {
      let StackFrame { state, parent, current } = self.stack.pop().unwrap();
      debug_assert!(current + 1 == parent.len());
      if let Syntax { primary: Primary::Alias(id), .. } = self.current().syntax() {
        println!("~ ended: {}", id);
        self.events_push(state.event(EventKind::End(id.clone())));
      }
      self.current_mut().match_begin = state.match_begin;
      self.current_mut().location = state.location;
    }
    return;
  }

  pub fn events_push(&mut self, e: Event<ID, E>) {
    Event::append(&mut self.event_buffer, e);
  }

  pub fn events_flush_to<H: FnMut(Event<ID, E>)>(&mut self, handler: &mut H) {
    while !self.event_buffer.is_empty() {
      (handler)(self.event_buffer.remove(0));
    }
  }

  pub fn min_match_begin(&self) -> usize {
    self.stack.iter().map(|sf| sf.state.match_begin).min().unwrap()
  }

  pub fn on_buffer_shrunk(&mut self, amount: usize) {
    for sf in &mut self.stack {
      sf.state.match_begin -= amount;
    }
  }

  fn get_definition(id: &ID, schema: &'s Schema<ID, E>) -> Result<E, &'s Vec<Syntax<ID, E>>> {
    if let Some(Syntax { primary: Primary::Seq(seq), repetition, .. }) = schema.get(id) {
      debug_assert!(!seq.is_empty());
      debug_assert!(*repetition.start() == 1 && *repetition.end() == 1);
      Ok(seq)
    } else {
      Err(Error::UndefinedID(id.to_string()))
    }
  }
}

#[derive(Clone, Debug)]
struct StackFrame<'s, ID, E: Item>
where
  ID: Clone + Display + Debug,
{
  state: State<'s, ID, E>,
  parent: &'s Vec<Syntax<ID, E>>,
  current: usize,
}

impl<'s, ID, E: Item> StackFrame<'s, ID, E>
where
  ID: Clone + Hash + Ord + Display + Debug,
{
  pub fn new(parent: &'s Vec<Syntax<ID, E>>) -> Self {
    debug_assert!(!parent.is_empty());
    let state = State::new(&parent[0]);
    Self { state, parent, current: 0 }
  }
}

/// The `Cursor` advances step by step, evaluating [`Syntax`] matches.
///
#[derive(Clone, Debug)]
pub struct State<'s, ID, E: Item>
where
  ID: Clone + Display + Debug,
{
  pub location: E::Location,
  pub match_begin: usize,
  pub match_length: usize,
  pub appearances: usize,

  /// The [`Syntax`] must be `Syntax::Seq`.
  syntax: &'s Syntax<ID, E>,
}

impl<'s, ID, E: 'static + Item> State<'s, ID, E>
where
  ID: Clone + Display + Debug,
{
  pub fn new(syntax: &'s Syntax<ID, E>) -> Self {
    Self { location: E::Location::default(), match_begin: 0, match_length: 0, appearances: 0, syntax }
  }

  pub fn syntax(&self) -> &'s Syntax<ID, E> {
    self.syntax
  }

  pub fn matches(&mut self, buffer: &[E], eof: bool) -> Result<E, Matching<ID, E>> {
    debug_assert!(buffer.len() >= self.match_begin + self.match_length);

    let items = &buffer[self.match_begin..];
    let reps = &self.syntax.repetition;
    debug_assert!(self.appearances <= *reps.end());
    if !self.can_repeate_more() {
      println!("~ matched: {}({}) -> no data", self.syntax(), E::debug_symbols(items),);
      return Ok(Matching::Match(0, None));
    }

    let matcher = if let Primary::Term(matcher) = &self.syntax.primary {
      matcher
    } else {
      unreachable!("Current syntax is not Primary::Term(matcher): {:?}", self.syntax)
    };

    let result = match matcher.matches(items)? {
      MatchResult::UnmatchAndCanAcceptMore if eof => MatchResult::Unmatch,
      MatchResult::MatchAndCanAcceptMore(length) if eof => MatchResult::Match(length),
      result => result,
    };

    let result = match result {
      MatchResult::Match(length) => {
        self.match_length = length;
        let values = self.extract(buffer).to_vec();
        println!("~ matched: {}({}) -> [{}]", self.syntax(), E::debug_symbols(items), E::debug_symbols(&values));
        Matching::Match(length, Some(self.event(EventKind::Fragments(values))))
      }
      MatchResult::Unmatch => {
        println!("~ unmatched: {}({})", self.syntax(), E::debug_symbols(items),);
        Matching::Unmatch
      }
      MatchResult::MatchAndCanAcceptMore(_) | MatchResult::UnmatchAndCanAcceptMore => Matching::More,
    };

    Ok(result)
  }

  pub fn can_repeate_more(&self) -> bool {
    if self.appearances == *self.syntax.repetition.end() {
      false
    } else {
      debug_assert!(self.appearances < *self.syntax.repetition.end());
      true
    }
  }

  fn proceed_along_buffer(&mut self, buffer: &[E]) {
    if self.match_length > 0 {
      self.location.increment_with_seq(self.extract(buffer));
      self.match_begin += self.match_length;
      self.match_length = 0;
    }
  }

  pub fn extract<'a>(&self, buffer: &'a [E]) -> &'a [E] {
    &buffer[self.match_begin..][..self.match_length]
  }

  pub fn event(&self, kind: EventKind<ID, E>) -> Event<ID, E> {
    Event { location: self.location, kind }
  }
}

pub enum Matching<ID, E: Item>
where
  ID: Clone + Display + Debug,
{
  Match(usize, Option<Event<ID, E>>),
  More,
  Unmatch,
}