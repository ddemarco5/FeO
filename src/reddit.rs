// Formatting
use std::fmt;
// For reddit functions
use roux::User;
use roux::util::RouxError;

#[derive(Debug, Clone)]
pub struct SnifferPost {
    pub title: String,
    pub body: String,
    pub subreddit: String,
    pub url: Option<String>
}


impl SnifferPost {
    pub fn from_roux(roux: roux::subreddit::responses::SubmissionsData) -> SnifferPost {
        debug!("creating a new sniffer post object");
        SnifferPost {
            title: roux.title,
            body: roux.selftext,
            subreddit: roux.subreddit,
            url: roux.url
        }
    }
}


pub struct RedditScraper {
    the_sniffer: roux::User,
    last_post_timestamp: f64,
}

impl fmt::Display for SnifferPost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.url {
            Some(m) => {
                write!(f, "{}, {}, {}, {}", self.title, self.body, self.subreddit, m)
            }
            None => {
                write!(f, "{}, {}, {}, No url", self.title, self.body, self.subreddit)
            }
        }
        
    }
}

impl RedditScraper {

    pub fn new(sniffer: String) -> RedditScraper {
        debug!("Created the reddit scraper");
        RedditScraper {
            the_sniffer: User::new(sniffer.as_str()),
            last_post_timestamp: 0.0,
        }
    }
    
    pub async fn update(&mut self) -> Result<Option<Vec<SnifferPost>>, RouxError> {

        info!("Updating reddit posts");

        // Get from reddit api
        let reddit_posts = self.the_sniffer.submitted().await;
        let reddit_posts = match reddit_posts {
            Ok(submissions_data) => submissions_data,
            Err(error) => {
                    error!("Encountered an error grabbing reddit posts\n{}", error);
                    return Err(error)
                },
        };

        let old_timestamp = self.last_post_timestamp;

        let mut posts = reddit_posts.data.children;

        // Sort oldest to newest
        posts.sort_by(|a, b| a.data.created.partial_cmp(&b.data.created).unwrap());

        // New vec
        let mut new_posts: Vec<SnifferPost> = Vec::new();

        // Add the sniffer's posts if they are newer than what we have
        for post in posts {
            let post_data = post.data;
            if post_data.created > self.last_post_timestamp {
                // We've got a new one! update our latest timestamp
                debug!("added a sniffer post with timestamp {}", post_data.created);
                self.last_post_timestamp = post_data.created;
                let sniffer_post = SnifferPost::from_roux(post_data);
                debug!("post text is {}:", sniffer_post.clone());
                new_posts.push(sniffer_post);
            }
        }
        // We've got a new one
        if old_timestamp != self.last_post_timestamp {
            return Ok(Some(new_posts));
        }
        return Ok(None);
    }

}