use logos::{Logos, Span};

use std::sync::Arc;
use tokio::sync::Mutex;
use crate::audio::player::AudioPlayer;

use serenity::model::channel::Message;
use serenity::prelude::Context;


#[derive(Logos, Debug, PartialEq, Clone)]
pub enum Token {
    #[token("help")]
    Help,
    #[token("leave")]
    Leave,
    #[token("stop")]
    Stop,
    #[token("pause")]
    Pause,
    #[token("resume")]
    Resume,
    #[token("skip")]
    Skip,
    #[token("list")]
    List,
    #[token("clear")]
    Clear,
    #[token("play")]
    Play,
    #[token("driveby")]
    Driveby,
    #[token("queue")]
    Queue,
    #[token("next")]
    Next,
    #[token("rm")]
    Rm,
    #[token("goto")]
    Goto,

    #[regex("[\\S]+", |lex| String::from(lex.slice()))] // regex match any non whitespace
    Generic(String),

    // Logos requires one token variant to handle errors,
    // it can be named anything you wish.
    #[error]
    // We can also use this variant to define whitespace,
    // or any other matches we wish to skip.
    #[regex(r"[ \t\n\f]+", logos::skip)]
    Error,
    Argument, // token used to signify a single argument
    Arguments, // The token used to signify that we expect infininte arguments
}

fn get_tokens(string: &String) -> Vec<(Token, Span)> {
    return Token::lexer(string).spanned().collect(); // Drop the span, we don't care about it
}


pub fn tokenize(string: &String) -> Result<(Vec<Token>, Option<Vec<Token>>), String> {
    let tokenized = get_tokens(string);
    let tokens: Vec<Token> = tokenized.iter().map(|x| x.0.clone()).collect(); // Collect all the Tokens into a vector, drop the span
    // Big yucky, but it goes through tokens and keeps everything that's a generic into a new vec
    let args = tokens.iter().cloned().filter(|x| { if let Token::Generic(_) = x { return true } false } ).collect();

    if tokens.is_empty() {
        return Err(String::from("No tokens parsed in string"));
    }
    return Ok((tokens, Some(args)));
}

#[derive(Clone)]
pub struct Parser {
    audio_player: Arc<Mutex<AudioPlayer>>,
}
impl Parser {
    pub fn new(player_arc: Arc<Mutex<AudioPlayer>>) -> Parser {
        return Parser {
            audio_player: player_arc,
        }
    }

    // Our token matching function
    fn match_tokens(&self, msg: &Message) -> Result<(Token, Option<Vec<Token>>), String> {
        
        let (tokens, generic_args) = tokenize(&msg.content)?;
        trace!("Tokens: {:?}", tokens);
        trace!("Args: {:?}", generic_args);
        
        'outer: for token_array in AudioCommands::EXPECTED_TOKENS { // Loop through our 2d array of known good token chains
            let mut parsed_tokens_iter = tokens.clone().into_iter().peekable();
            let currently_checking_token = token_array[0].clone();
            trace!("Currently checking out token string for {:?}", token_array);
            for token in *token_array { // Loop through each token in array
                trace!("Working on {:?}", token);
                // If we've gotten to Arguments, break out to process N amount of arguments
                // Verify the rest of the parsed tokens iterator and break out of the loop
                if *token == Token::Arguments { 
                    trace!("Processing infinite argument token");
                    if parsed_tokens_iter.peek().is_none(){ 
                        trace!("No tokens to process, expecting at least more than 0");
                        // check other commands
                        continue 'outer;
                    }
                    while let Some(t) = parsed_tokens_iter.next() { // While there's something in the iterator
                        if let Token::Generic(_) = t {
                            trace!("Got a generic where we expected it");
                        }
                        else {
                            trace!("Didn't get a generic when we expected, found {:?}", t);
                            continue 'outer;
                        }
                    }
                    // if we get here they were all valid, break out of the checking loop
                    break;
                }
                // Make sure we have another token in our parsed list
                match parsed_tokens_iter.next() {
                    Some(parsed_token) => {
                        trace!("comparing expected: {:?} -- against : {:?}", token, parsed_token);
                        // Check if our token is a generic first
                        if let Token::Generic(_) = parsed_token {
                            if *token != Token::Argument {
                                // This means we don't have a valid match, as a generic is counted as a single argument
                                trace!("{:?} isn't a generic, continuing", parsed_token);
                                continue 'outer;
                            }
                        }
                        // If not a generic match specific token
                        else if parsed_token != *token {
                            trace!("{:?} and {:?} didn't match, continuing", parsed_token, *token);
                            continue 'outer;
                        }
                        // Otherwise we've matched a token, and we keep going
                    }
                    None => {
                        trace!("Ran out of parsed tokens, can't match {:?}, continuing", currently_checking_token);
                        continue 'outer;
                    },
                    
                }
            }
            // if we reach here, we've successfully matched a whole token chain
            // Make sure there's nothing left, making it a bad command with extra args
            if parsed_tokens_iter.peek().is_some(){ 
                return Err(String::from("Matched a valid command, but we still have parsed tokens, making it bad"));
            }
            return Ok((currently_checking_token, generic_args));     
        }
        Err(String::from("No valid token chain has been found"))
    }

    // Our function matching table
    pub async fn process(&self, ctx: &Context, msg: &Message) -> Result<(), String> {
        let (matched, args) = self.match_tokens(msg)?;
        warn!("Matched {:?} with args {:?}", matched, args);
        match matched {
            Token::Help => {
                let locked_player = self.audio_player.lock().await;
                locked_player.print_help(ctx)?;
            }
            Token::List => {
                let locked_player = self.audio_player.lock().await;
                locked_player.print_queue(ctx)?;
            },
            Token::Pause => {
                let locked_player = self.audio_player.lock().await;
                locked_player.pause_locking()?;
            },
            Token::Resume => {
                let locked_player = self.audio_player.lock().await;
                locked_player.resume_locking()?;

            },
            Token::Skip => {
                let locked_player = self.audio_player.lock().await;
                locked_player.skip_locking()?;

            },
            Token::Clear => {
                let locked_player = self.audio_player.lock().await;
                locked_player.clear_queue_locking()?;

            },
            Token::Stop => {
                let locked_player = self.audio_player.lock().await;
                locked_player.stop_locking()?;

            },
            Token::Leave => {
                let mut locked_player = self.audio_player.lock().await;
                locked_player.hangup()?;

            },
            Token::Play => {
                let mut locked_player = self.audio_player.lock().await;
                locked_player.process_play(&ctx, &msg, args.unwrap()).await?;

            },
            Token::Driveby => {
                let mut locked_player = self.audio_player.lock().await;
                locked_player.process_driveby(&ctx, &msg, args.unwrap()).await?;

            },
            Token::Queue => {
                let mut locked_player = self.audio_player.lock().await;
                locked_player.process_enqueue(&ctx, &msg, args.unwrap()).await?;

            },
            Token::Next => {
                let mut locked_player = self.audio_player.lock().await;
                locked_player.process_next(&ctx, &msg, args.unwrap()).await?;

            },
            Token::Goto => {
                let locked_player = self.audio_player.lock().await;
                locked_player.process_goto(args.unwrap()).await?;
                

            },
            Token::Rm => {
                let mut locked_player = self.audio_player.lock().await;
                locked_player.process_rm(args.unwrap()).await?;

            },
            _ => {
                return Err(String::from(format!("Found a token that isn't in the table??? should never happen: {:?}", matched)));
            }
        }
        Ok(())
    }

}

trait Process {
    fn process(&self) -> Result<(), String>;
}

// Separate object in case we want commands for different sniffer components

// This is the audio one in particular
struct AudioCommands;
impl AudioCommands {
    const EXPECTED_TOKENS: &'static [&'static [Token]] = &[
        &[Token::Help],
        &[Token::List],
        &[Token::Pause],
        &[Token::Resume],
        &[Token::Skip],
        &[Token::Clear],
        &[Token::Stop],
        &[Token::Leave],
        &[Token::Play, Token::Argument],
        &[Token::Driveby, Token::Argument],
        &[Token::Queue, Token::Arguments],
        &[Token::Next, Token::Arguments],
        &[Token::Goto, Token::Argument],
        &[Token::Rm, Token::Arguments],
    ];
}