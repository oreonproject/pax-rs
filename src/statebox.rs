use std::{any::Any, collections::HashMap};

#[derive(Debug)]
pub struct StateBox {
    store: HashMap<String, Box<dyn Any>>,
}

impl StateBox {
    pub fn new() -> Self {
        StateBox {
            store: HashMap::new(),
        }
    }
    pub fn insert<T: 'static>(&mut self, key: &str, value: T) -> Result<(), String> {
        if self.store.contains_key(key) {
            return Err(String::from(
                "Key already exists! If you wish to update this value, use `set()` method instead.",
            ));
        }
        self.store.insert(key.to_string(), Box::new(value));
        Ok(())
    }
    pub fn remove(&mut self, key: &str) -> Result<(), String> {
        if self.store.remove_entry(key).is_some() {
            Ok(())
        } else {
            Err(String::from("Cannot remove nonexistant key!"))
        }
    }
    pub fn get<T: 'static>(&self, key: &str) -> Option<&T> {
        self.store.get(key)?.downcast_ref::<T>()
    }
    pub fn set<T: 'static>(&mut self, key: &str, value: T) -> Result<(), String> {
        if let Some(state) = self.store.get_mut(key) {
            *state = Box::new(value);
            Ok(())
        } else {
            Err(String::from(
                "Key not found. If you wish to create this value, use `insert()` method instead.",
            ))
        }
    }
    pub fn push<T: 'static>(&mut self, _key: &str, _value: T) -> ! {
        //Learned the '!' (bang) return type from RUst Kernel dev ;P
        unimplemented!()
    }
    pub fn pop<T: 'static>(&mut self, key: &str) -> Option<T> {
        self.store
            .remove(key)?
            .downcast::<T>()
            .map(|x| Some(*x))
            .ok()?
    }
    pub fn shove<T: 'static>(&mut self, key: &str, value: T) {
        if let Some(state) = self.store.get_mut(key) {
            *state = Box::new(value)
        } else {
            self.store.insert(key.to_string(), Box::new(value));
        }
    }
    pub fn yank(&mut self, key: &str) {
        // WARNING: This function has VERY different connotation to the 'yank' from NVIM!
        self.store.remove(key);
    }
    pub fn len(&self) -> usize {
        self.store.len()
    }
    // This is to make Clippy happy
    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }
}

impl Default for StateBox {
    fn default() -> Self {
        Self::new()
    }
}
