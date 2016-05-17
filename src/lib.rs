// TODO:
//
// panic here or panic there?
//  * catch_unwind is 6x slower than a normal call with panic=abort
//      * can be 3x if we deal with PANIC_COUNT
//  * catch_unwind is 7x slower than a normal call with panic=unwind
//  * perspective, allocation is 20x slower than a noop call
//  * also attributed to the indirect call
//  * data point - wangle deals with C++ exceptions
//
// select() and returning a future back
//  * maybe this is just streams...

mod cell;
mod slot;
mod util;

mod error;
pub use error::{PollError, PollResult, FutureError, FutureResult};

pub mod executor;

// Primitive futures
mod collect;
mod done;
mod empty;
mod failed;
mod finished;
mod lazy;
mod promise;
pub use collect::{collect, Collect};
pub use done::{done, Done};
pub use empty::{empty, Empty};
pub use failed::{failed, Failed};
pub use finished::{finished, Finished};
pub use lazy::{lazy, Lazy};
pub use promise::{promise, Promise, Complete};

// combinators
mod and_then;
mod flatten;
mod join;
mod map;
mod map_err;
mod or_else;
mod select;
mod select2;
mod then;
pub use and_then::AndThen;
pub use flatten::Flatten;
pub use join::Join;
pub use map::Map;
pub use map_err::MapErr;
pub use or_else::OrElse;
pub use select::Select;
pub use select2::{Select2, Select2Next};
pub use then::Then;

// streams
pub mod stream;

// impl details
mod chain;
mod impls;
mod forget;

// TODO: Send + 'static is annoying, but required by cancel and_then, document
// TODO: not object safe
//
// FINISH CONDITIONS
//      - poll() return Some
//      - await() is called
//      - schedule() is called
//      - schedule_boxed() is called
//
// BAD:
//      - doing any finish condition after an already called finish condition
//
// WHAT HAPPENS
//      - panic?
pub trait Future: Send + 'static {
    type Item: Send + 'static;
    type Error: Send + 'static;

    // returns None - you can keep calling this, future is not consumed
    // returns Some - future becomes consumed
    //
    // If future is consumed then this returns `Some` of a panicked error.
    //
    // TODO: why does this actually exist?
    // fn poll(&mut self) -> Option<PollResult<Self::Item, Self::Error>>;

    // - If future is not consumes, causes future calls to poll() and schedule()
    //   to return immediately with Canceled
    // - If future is consumed via poll(), does nothing, but causes future
    //   poll()/schedule() invocations to return immediately with Canceled
    // - If future is consumed via schedule(), arranges to have the callback
    //   called "as soon as possible" with a resolution. That resolution may be
    //   Canceled, or it may be anything else (depending on how the race plays
    //   out).
    //
    // Canceling a canceled future doesn't do much, shouldn't panic either.
    //
    // FAQ:
    //
    // Q: Why is this not drop?
    // A: How to differentiate drop() vs cancel() then drop()
    // fn cancel(&mut self);

    // Contract: the closure `f` is guaranteed to get called
    //
    // - If future is consumed, `f` is immediately called with a "panicked"
    //   result.
    // - If future is not consumed, arranges `f` to be called with the resolved
    //   value. May be called earlier if `cancel` is called.
    //
    // This function will "consume" the future.
    fn schedule<F>(&mut self, f: F)
        where F: FnOnce(PollResult<Self::Item, Self::Error>) + Send + 'static,
              Self: Sized;

    // Impl detail, just do this as:
    //
    //      self.schedule(|r| f.call(r))
    fn schedule_boxed(&mut self, f: Box<Callback<Self::Item, Self::Error>>);

    // TODO: why can't this be in this lib?
    //
    // Seems not very useful if we can't provide it.
    // fn await(&mut self) -> FutureResult<Self::Item, Self::Error>;

    fn boxed(self) -> Box<Future<Item=Self::Item, Error=Self::Error>>
        where Self: Sized
    {
        Box::new(self)
    }

    fn map<F, U>(self, f: F) -> Map<Self, F>
        where F: FnOnce(Self::Item) -> U + Send + 'static,
              U: Send + 'static,
              Self: Sized,
    {
        assert_future::<U, Self::Error, _>(map::new(self, f))
    }

    fn map2<F, U>(self, f: F) -> Box<Future<Item=U, Error=Self::Error>>
        where F: FnOnce(Self::Item) -> U + Send + 'static,
              U: Send + 'static,
              Self: Sized,
    {
        self.then(|r| r.map(f)).boxed()
    }

    fn map_err<F, E>(self, f: F) -> MapErr<Self, F>
        where F: FnOnce(Self::Error) -> E + Send + 'static,
              E: Send + 'static,
              Self: Sized,
    {
        assert_future::<Self::Item, E, _>(map_err::new(self, f))
    }

    fn map_err2<F, E>(self, f: F) -> Box<Future<Item=Self::Item, Error=E>>
        where F: FnOnce(Self::Error) -> E + Send + 'static,
              E: Send + 'static,
              Self: Sized,
    {
        self.then(|res| res.map_err(f)).boxed()
    }

    fn then<F, B>(self, f: F) -> Then<Self, B, F>
        where F: FnOnce(Result<Self::Item, Self::Error>) -> B + Send + 'static,
              B: IntoFuture,
              Self: Sized,
    {
        assert_future::<B::Item, B::Error, _>(then::new(self, f))
    }

    fn and_then<F, B>(self, f: F) -> AndThen<Self, B, F>
        where F: FnOnce(Self::Item) -> B + Send + 'static,
              B: IntoFuture<Error = Self::Error>,
              Self: Sized,
    {
        assert_future::<B::Item, Self::Error, _>(and_then::new(self, f))
    }

    fn and_then2<F, B>(self, f: F) -> Box<Future<Item=B::Item, Error=Self::Error>>
        where F: FnOnce(Self::Item) -> B + Send + 'static,
              B: IntoFuture<Error = Self::Error>,
              Self: Sized,
    {
        self.then(|res| {
            match res {
                Ok(e) => f(e).into_future().boxed(),
                Err(e) => failed(e).boxed(),
            }
        }).boxed()
    }

    fn or_else<F, B>(self, f: F) -> OrElse<Self, B, F>
        where F: FnOnce(Self::Error) -> B + Send + 'static,
              B: IntoFuture<Item = Self::Item>,
              Self: Sized,
    {
        assert_future::<Self::Item, B::Error, _>(or_else::new(self, f))
    }

    fn or_else2<F, B>(self, f: F) -> Box<Future<Item=B::Item, Error=B::Error>>
        where F: FnOnce(Self::Error) -> B + Send + 'static,
              B: IntoFuture<Item = Self::Item>,
              Self: Sized,
    {
        self.then(|res| {
            match res {
                Ok(e) => finished(e).boxed(),
                Err(e) => f(e).into_future().boxed(),
            }
        }).boxed()
    }

    fn select<B>(self, other: B) -> Select<Self, B::Future>
        where B: IntoFuture<Item=Self::Item, Error=Self::Error>,
              Self: Sized,
    {
        let f = select::new(self, other.into_future());
        assert_future::<Self::Item, Self::Error, _>(f)
    }

    fn select2<B>(self, other: B) -> Select2<Self, B::Future>
        where B: IntoFuture<Item=Self::Item, Error=Self::Error>,
              Self: Sized,
    {
        let f = select2::new(self, other.into_future());
        assert_future::<(Self::Item, Select2Next<Self, B::Future>),
                        (Self::Error, Select2Next<Self, B::Future>),
                        _>(f)
    }

    fn join<B>(self, other: B) -> Join<Self, B::Future>
        where B: IntoFuture<Error=Self::Error>,
              Self: Sized,
    {
        let f = join::new(self, other.into_future());
        assert_future::<(Self::Item, B::Item), Self::Error, _>(f)
    }

    fn flatten(self) -> Flatten<Self>
        where Self::Item: IntoFuture,
              <<Self as Future>::Item as IntoFuture>::Error:
                    From<<Self as Future>::Error>,
              Self: Sized
    {
        let f = flatten::new(self);
        assert_future::<<<Self as Future>::Item as IntoFuture>::Item,
                        <<Self as Future>::Item as IntoFuture>::Error,
                        _>(f)
    }

    fn flatten2(self) -> Box<Future<Item=<<Self as Future>::Item as IntoFuture>::Item,
                                    Error=<<Self as Future>::Item as IntoFuture>::Error>>
        where Self::Item: IntoFuture,
              <<Self as Future>::Item as IntoFuture>::Error:
                    From<<Self as Future>::Error>,
              Self: Sized
    {
        self.then(|res| {
            match res {
                Ok(e) => e.into_future().boxed(),
                Err(e) => failed(From::from(e)).boxed(),
            }
        }).boxed()
    }

    fn forget(self) where Self: Sized {
        forget::forget(self);
    }
}

fn assert_future<A, B, F>(t: F) -> F
    where F: Future<Item=A, Error=B>,
          A: Send + 'static,
          B: Send + 'static,
{
    t
}

pub trait Callback<T, E>: Send + 'static {
    fn call(self: Box<Self>, result: PollResult<T, E>);
}

impl<F, T, E> Callback<T, E> for F
    where F: FnOnce(PollResult<T, E>) + Send + 'static
{
    fn call(self: Box<F>, result: PollResult<T, E>) {
        (*self)(result)
    }
}

pub trait IntoFuture: Send + 'static {
    type Future: Future<Item=Self::Item, Error=Self::Error>;
    type Item: Send + 'static;
    type Error: Send + 'static;

    fn into_future(self) -> Self::Future;
}

impl<F: Future> IntoFuture for F {
    type Future = F;
    type Item = F::Item;
    type Error = F::Error;

    fn into_future(self) -> F {
        self
    }
}
