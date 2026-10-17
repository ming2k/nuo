//! Component Story Specification and Offline Harness (ADR-0073).

use nuotc::{Frame, Rect};
use crossterm::event::KeyEvent;
use crate::theme::Theme;
use super::Component;

/// Outcome of a story-level interactive event dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoryEventOutcome {
    /// The event was consumed and state was mutated.
    Consumed,
    /// The event was ignored by the component.
    Ignored,
}

/// A concrete, immutable scenario fixture.
pub struct Story<P, S> {
    pub name: &'static str,
    pub description: &'static str,
    pub props: P,
    pub initial_state: S,
}

impl<P, S> Story<P, S> {
    pub fn new(name: &'static str, description: &'static str, props: P, initial_state: S) -> Self {
        Self {
            name,
            description,
            props,
            initial_state,
        }
    }
}

/// Trait for components that declare canonical verification stories (ADR-0073).
pub trait StorybookComponent: Component {
    /// Category partition ("Overlays", "Disclosure", "Chrome", "Tools").
    fn category() -> &'static str;
    /// Human-readable component name.
    fn name() -> &'static str;
    /// Suite of canonical edge-case scenarios covering standard, overflow,
    /// truncation, error, and streaming states (`[INV-TUI-STORY-02]`).
    fn stories() -> Vec<Story<Self::Props, Self::State>>;

    /// Map a raw terminal key event to the component's internal `Msg`.
    fn map_key_to_msg(
        _key: &KeyEvent,
        _props: &Self::Props,
        _state: &Self::State,
    ) -> Option<Self::Msg> {
        None
    }
}

/// Type-erased story instance for the interactive workbench and visual regression runners.
pub trait AnyStory: Send {
    fn category(&self) -> &'static str;
    fn component_name(&self) -> &'static str;
    fn story_name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme);
    fn handle_key(&mut self, key: &KeyEvent) -> StoryEventOutcome;
    fn reset(&mut self);
}

/// Concrete typed implementation of `AnyStory`.
pub struct ComponentStoryInstance<C: StorybookComponent + 'static> {
    props: C::Props,
    state: C::State,
    initial_state: C::State,
    name: &'static str,
    description: &'static str,
}

impl<C: StorybookComponent + 'static> ComponentStoryInstance<C> {
    pub fn new(story: Story<C::Props, C::State>) -> Self {
        Self {
            props: story.props,
            state: story.initial_state.clone(),
            initial_state: story.initial_state,
            name: story.name,
            description: story.description,
        }
    }
}

impl<C: StorybookComponent + 'static> AnyStory for ComponentStoryInstance<C> {
    fn category(&self) -> &'static str {
        C::category()
    }

    fn component_name(&self) -> &'static str {
        C::name()
    }

    fn story_name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        C::render(&self.props, &self.state, frame, area, theme);
    }

    fn handle_key(&mut self, key: &KeyEvent) -> StoryEventOutcome {
        if let Some(msg) = C::map_key_to_msg(key, &self.props, &self.state) {
            let _ = C::update(&self.props, &mut self.state, msg);
            StoryEventOutcome::Consumed
        } else {
            StoryEventOutcome::Ignored
        }
    }

    fn reset(&mut self) {
        self.state = self.initial_state.clone();
    }
}

/// Erased helper to create a boxed `AnyStory` from a component.
pub fn instantiate_story<C: StorybookComponent + 'static>(
    story: Story<C::Props, C::State>,
) -> Box<dyn AnyStory> {
    Box::new(ComponentStoryInstance::<C>::new(story))
}
