use std::path::PathBuf;
use structopt::clap::AppSettings;
use structopt::StructOpt;

#[derive(StructOpt)]
#[structopt(global_settings = &[AppSettings::ColoredHelp])]
pub struct Args {
    /// Only show warnings
    #[structopt(short, long, global = true)]
    pub quiet: bool,
    /// More verbose logs
    #[structopt(short, long, global = true, parse(from_occurrences))]
    pub verbose: u8,
    /// The index that should be signed
    #[structopt(long)]
    pub index_path: PathBuf,
    /// The path the signed index should be written to, may be equal to input
    #[structopt(long)]
    pub output_path: PathBuf,
    #[structopt(subcommand)]
    pub subcommand: SubCommand,
}

#[derive(StructOpt)]
pub enum SubCommand {
    /// Copy the signature from a APKINDEX.tar.gz inside an image
    FromImage {
        /// Path to the image
        path: PathBuf,
        /// The architecture
        #[structopt(long)]
        arch: String,
    },
    /// Copy the signature from another signed APKINDEX.tar.gz
    FromIndex {
        /// Path to APKINDEX.tar.gz
        path: PathBuf,
    },
    /// Copy the signature from a file
    FromFile {
        /// Path to the existing signature
        path: PathBuf,
    },
}
