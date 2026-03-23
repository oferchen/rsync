mod loader;
mod parser;
mod resolver;

#[cfg(test)]
mod tests;

pub(crate) use loader::load_file_list_operands;
#[cfg(test)]
pub(crate) use loader::read_file_list_from_reader;
#[cfg(test)]
pub(crate) use parser::transfer_requires_remote;
pub(crate) use parser::{operand_is_remote, resolve_files_from_source};
pub(crate) use resolver::resolve_file_list_entries;
