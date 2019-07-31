#![allow(warnings)]

use rayon;
use std::{
    borrow::BorrowMut,
    collections::BTreeMap,
    ops::{Deref, DerefMut},
};

#[derive(PartialEq, Eq, Debug)]
struct Device(pub u32);
#[derive(PartialEq, Eq, Debug)]
struct DepthImage(pub u32);
#[derive(PartialEq, Eq, Debug)]
struct Set(pub u32);

struct Renderer {
    device: Device,
    depth_image: DepthImage,
    set: Set,
}

#[macro_export]
macro_rules! par_custom {
    (@join write [$($write:ident)*] read [$($read:ident)*] => $do:block) => {
        {
            $(
                let $write = $write.borrow_mut();
            )*
            $(
                let $read = &$read;
            )*
            move || { $do; () }
        }
    };
    (@join write [$($head_write:ident)*] read [$($head_read:ident)*] => $head_do:block write [$($tail_write:ident)*] read [$($tail_read:ident)*] => $tail_do:block) => {
        rayon::join(
            par_custom!(@join write [$($head_write)*] read [$($head_read)*] => $head_do),
            par_custom!(@join write [$($tail_write)*] read [$($tail_read)*] => $tail_do),
        );
    };
    (@join write [$($first_write:ident)*] read [$($first_read:ident)*] => $first_do:block write [$($second_write:ident)*] read [$($second_read:ident)*] => $second_do:block $(write [$($tail_write:ident)*] read [$($tail_read:ident)*] => $tail_do:block),+) => {
        rayon::join(
            par_custom!(@join write [$($first_write)*] read [$($first_read)*] => $first_do),
            move || { par_custom!(@join write [$($second_write)*] read [$($second_read)*] => $second_do $( write [$($tail_write)*] read [$($tail_read)*] => $tail_do),+) },
        );
    };
    // TODO: should not accept a single job to run
    ($(write [$($write:ident)*] read [$($read:ident)*] => $do:block)+) => {
        par_custom!(@join $(write [$($write)*] read [$($read)*] => $do)+);
    };
}

#[macro_export]
macro_rules! seq_custom {
    ($(write [$($write:ident)*] read [$($read:ident)*] => $do:tt)*) => {
        $(
            {
                $(let $write = $write.borrow_mut();)*
                $(let $read = &$read;)*
                $do;
            }
        )*
    };
}

pub struct EntitiesStorage {
    pub alive: croaring::Bitmap,
}

pub struct ComponentStorage<T> {
    pub alive: croaring::Bitmap,
    pub data: BTreeMap<u32, T>,
}

impl<T> ComponentStorage<T> {
    pub fn new() -> ComponentStorage<T> {
        ComponentStorage {
            alive: croaring::Bitmap::create(),
            data: BTreeMap::new(),
        }
    }
}

#[derive(PartialEq, Clone, Debug)]
struct Position(glm::Vec3);

#[test]
fn test_components() {
    let mut entities = EntitiesStorage {
        alive: croaring::Bitmap::create(),
    };
    let mut positions = ComponentStorage::<glm::Vec3> {
        alive: croaring::Bitmap::create(),
        data: BTreeMap::new(),
    };
    let mut velocity = ComponentStorage::<glm::Vec3> {
        alive: croaring::Bitmap::create(),
        data: BTreeMap::new(),
    };
    let mut timedelta = 0.0f32;

    entities.alive.add_range(0..5);
    positions.alive.add_many(&[0, 3, 4, 8]);

    seq_custom! {
        write [velocity timedelta] read [entities] => {
            *timedelta = 3.0;
            par_custom! {
                write [timedelta] read [] => {
                    *timedelta = 3.0;
                    seq_custom! {
                        write [timedelta] read [] => {
                            *timedelta = 5.0;
                        }
                    }
                }
                write [velocity] read [entities] => {
                    for x in entities.alive.iter() {
                        velocity.alive.add(x);
                        velocity.data.insert(x, glm::vec3(10.0, 20.0, 0.0));
                    }
                }
            }
        }
        write [positions] read [velocity entities timedelta] => {
            for ix in (&positions.alive & &velocity.alive & &entities.alive).iter() {
                *positions.data.entry(ix).or_insert(na::zero()) += velocity.data.get(&ix).unwrap() * *timedelta;
            }
        }
    }

    assert_eq!(timedelta, 5.0);
    assert_eq!(positions.data.get(&3), Some(&glm::vec3(50.0, 100.0, 0.0)));
}
