use axum::{
    error_handling::HandleErrorExt, http::StatusCode, routing::service_method_routing as service,
    Router,
};
use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_tasks::prelude::*;
use hotwatch::{
    blocking::{Flow, Hotwatch},
    Event,
};
use itertools::Itertools;
use serde_derive::{Deserialize, Serialize};
use serde_json::Value;
use slog::{error, info, o, Drain};
use std::net::SocketAddr;
use structopt::StructOpt;
use tera::Tera;
use tower_http::services::ServeDir;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::{path::Path, path::PathBuf};

#[derive(Debug, Clone, Deserialize)]
enum SourceType {
    // File will be copied to the corresponding path in the output dir directly
    StaticContent,
    // File will be loaded as a template available to Tera
    Template,
    // File will be loaded as a dynamic content source and processed accordingly
    DynamicContentSinglePage,
    // File will be loaded as a blog post
    DynamicContentBlogPost,
    // File will be loaded as a template for tag pages
    DynamicContentBlogpostTagPage,
    // File will be loaded as a template for archive pages
    DynamicContentBlogpostArchivePage,
    // File will be loaded as an RSS template
    DynamicContentBlogpostRssPage,
    // File will be loaded as sitemap page
    DynamicContentSitemap,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
enum DynamicContentType {
    // A single page that gets rendered
    SinglePage,
    // A blog post that gets rendered with markdown
    Blogpost,
    // A tag page
    BlogpostTagPage,
    // An archive page
    BlogpostArchivePage,
    // An rss page
    BlogpostRssPage,
    // A sitemap page
    SitemapPage,
}

// Immutable config loaded from the user
#[derive(Clone, Debug, Deserialize)]
struct Config {
    source_dir: PathBuf,
    output_dir: PathBuf,
    sitename: String,
    sources: HashMap<String, SourceType>,
    routes: HashMap<String, String>,
    blogpost_template: String,
    site_url: String,
}

struct LoadStaticContentGlob {
    glob: String,
}

struct LoadTemplateGlob {
    glob: String,
}

struct LoadDynamicContentGlob {
    glob: String,
    type_: DynamicContentType,
}

fn create_source_loaders(config: Res<Config>, mut commands: Commands) {
    for (glob, source) in &config.sources {
        match source {
            SourceType::StaticContent => {
                commands
                    .spawn()
                    .insert(LoadStaticContentGlob { glob: glob.clone() });
            }
            SourceType::Template => {
                commands
                    .spawn()
                    .insert(LoadTemplateGlob { glob: glob.clone() });
            }
            SourceType::DynamicContentSinglePage => {
                commands.spawn().insert(LoadDynamicContentGlob {
                    glob: glob.clone(),
                    type_: DynamicContentType::SinglePage,
                });
            }
            SourceType::DynamicContentBlogPost => {
                commands.spawn().insert(LoadDynamicContentGlob {
                    glob: glob.clone(),
                    type_: DynamicContentType::Blogpost,
                });
            }
            SourceType::DynamicContentBlogpostTagPage => {
                commands.spawn().insert(LoadDynamicContentGlob {
                    glob: glob.clone(),
                    type_: DynamicContentType::BlogpostTagPage,
                });
            }
            SourceType::DynamicContentBlogpostArchivePage => {
                commands.spawn().insert(LoadDynamicContentGlob {
                    glob: glob.clone(),
                    type_: DynamicContentType::BlogpostArchivePage,
                });
            }
            SourceType::DynamicContentBlogpostRssPage => {
                commands.spawn().insert(LoadDynamicContentGlob {
                    glob: glob.clone(),
                    type_: DynamicContentType::BlogpostRssPage,
                });
            }
            SourceType::DynamicContentSitemap => {
                commands.spawn().insert(LoadDynamicContentGlob {
                    glob: glob.clone(),
                    type_: DynamicContentType::SitemapPage,
                });
            }
        }
    }
}

fn make_relative(path: &Path, base: &Path) -> PathBuf {
    if path.is_absolute() {
        path.strip_prefix(base)
            .expect("Static content must be a prefix of base")
    } else {
        path
    }
    .to_path_buf()
}

fn static_content_source_loader(
    config: Res<Config>,
    query: Query<&LoadStaticContentGlob>,
    mut commands: Commands,
) {
    let paths = query.iter().flat_map(|glob| {
        glob::glob(&glob.glob)
            .expect(&format!("Unable to read glob: {}", &glob.glob))
            .filter_map(|p| p.ok())
    });
    for path in paths {
        let relative = make_relative(&path, config.source_dir.as_path());
        commands
            .spawn()
            .insert(RelativeSourcePath {
                path: relative.clone(),
            })
            .insert(URL {
                url: relative.to_string_lossy().to_string(),
                absolute: format!("{}{}", config.site_url, relative.to_string_lossy()),
            })
            .insert(RelativeOutputPath { path: relative })
            .insert(CopySourceToOutput {})
            .insert(IsStatiContent {});
    }
}

fn template_source_loader(query: Query<&LoadTemplateGlob>, mut commands: Commands) {
    let mut iter = query.iter();
    let tera = iter.next().map(|glob| {
        Tera::new(&glob.glob).expect(&format!("Unable to load templates from {}", glob.glob))
    });
    if tera.is_none() {
        return;
    }
    let mut tera = tera.unwrap();
    while let Some(glob) = iter.next() {
        let new =
            Tera::new(&glob.glob).expect(&format!("Unable to load templates from {}", glob.glob));
        tera.extend(&new).expect(&format!(
            "Unable to extend with templates from {}",
            glob.glob
        ));
    }
    commands.insert_resource(tera);
}

struct IsDynamicContent {}

#[derive(Debug, Clone, Deserialize)]
struct DynamicContentMetadata {
    route: String,
    title: String,
    template: Option<String>,
    #[serde(default)]
    navbar: Option<usize>,
    #[serde(default)]
    markdown: bool,
    #[serde(flatten)]
    stuff: HashMap<String, Value>,
    // OpenGraph metadata. Title is used above if og_title not set
    #[serde(default)]
    og_title: String,
    #[serde(default)]
    og_type: String,
    #[serde(default)]
    og_description: String,
}

#[derive(Debug, Clone)]
struct DynamicContentContents {
    contents: String,
}

fn dynamic_content_source_loader(
    config: Res<Config>,
    query: Query<&LoadDynamicContentGlob>,
    mut commands: Commands,
) {
    let paths = query.iter().flat_map(|glob| {
        glob::glob(&glob.glob)
            .expect(&format!("Unable to read glob: {}", &glob.glob))
            .filter_map(|p| p.ok())
            .map(|path| (glob.type_.clone(), path))
    });
    // TODO: Make this parallel somehow to speed up I/O
    for (type_, path) in paths {
        let relative = make_relative(&path, config.source_dir.as_path());
        let source = std::fs::read_to_string(&path)
            .expect(&format!("Unable to read file {}", path.to_string_lossy()));
        assert!(source.starts_with("{\n"));
        let token = "\n}\n\n";
        let split = source.find(token).expect(&format!(
            "Need terminator for metadata in {}!",
            path.to_string_lossy()
        ));
        let mut metadata: DynamicContentMetadata = serde_json::from_str(&source[0..split + 2])
            .expect(&format!(
                "Could not parse metadata in {}:",
                path.to_string_lossy()
            ));
        // TODO: See if we can avoid the copy here
        let contents = source[split + token.len()..].to_string();
        match type_ {
            DynamicContentType::Blogpost => {
                metadata.markdown = true;
                metadata.template = Some(config.blogpost_template.clone());
                metadata.stuff.insert(
                    "slug".to_string(),
                    relative
                        .file_stem()
                        .expect(&format!(
                            "Path must have a stem: {}",
                            relative.as_path().to_string_lossy()
                        ))
                        .to_string_lossy()
                        .to_string()
                        .into(),
                );
                let date = metadata
                    .stuff
                    .get("date")
                    .expect(&format!(
                        "Blogpost at {} is missing a date!",
                        relative.as_path().to_string_lossy()
                    ))
                    .as_str()
                    .expect(&format!(
                        "Blogpost at {} has a non-string date!",
                        relative.as_path().to_string_lossy()
                    ))
                    .to_string();
                let parts: Vec<_> = date.split("/").collect();
                match parts.as_slice() {
                    [year, month, day] => {
                        metadata
                            .stuff
                            .insert("year".to_string(), format!("{:4}", year).into());
                        metadata
                            .stuff
                            .insert("month".to_string(), format!("{:2}", month).into());
                        metadata
                            .stuff
                            .insert("day".to_string(), format!("{:2}", day).into());
                    }
                    _ => {
                        panic!(
                            "Blogpost at {} has an invalid date!",
                            relative.as_path().to_string_lossy()
                        )
                    }
                }
                metadata.og_type = "article".to_string();
                if let Some(excerpt) = metadata.stuff.get("excerpt") {
                    metadata.og_description =
                        excerpt.as_str().map(|s| s.to_owned()).unwrap_or_default();
                }
            }
            DynamicContentType::SinglePage
            | DynamicContentType::BlogpostTagPage
            | DynamicContentType::BlogpostArchivePage
            | DynamicContentType::BlogpostRssPage
            | DynamicContentType::SitemapPage => {}
        };
        commands
            .spawn()
            .insert(RelativeSourcePath { path: relative })
            .insert(IsDynamicContent {})
            .insert(metadata)
            .insert(DynamicContentContents { contents })
            .insert(type_);
    }
}

// URL (identifier) where this path will be at
// TODO: Maybe allow the single page ones to define routes inline
#[derive(Debug)]
struct URL {
    url: String,
    absolute: String,
}

fn url_for_impl(config: &Config, route: &String, replacements: &HashMap<String, Value>) -> URL {
    let mut url = config
        .routes
        .get(route)
        .expect(&format!("No route defined for {}", route))
        .clone();
    // Dynamic routes might need things replaced in from the stuff
    for (key, value) in replacements.iter() {
        if url.find("{").is_none() {
            break;
        }
        let to_replace = format!("{{{}}}", key);
        if let Some(value) = value.as_i64() {
            url = url.replace(&to_replace, &value.to_string());
        } else if let Some(value) = value.as_str() {
            url = url.replace(&to_replace, value);
        };
    }
    assert!(!url.contains("{"), "URL should be fully generated: {}", url);
    let absolute = format!("{}{}", config.site_url, url);
    URL { url, absolute }
}

fn metadata_to_url(config: &Config, metadata: &DynamicContentMetadata) -> URL {
    url_for_impl(config, &metadata.route, &metadata.stuff)
}

fn generate_urls(
    config: Res<Config>,
    query: Query<(Entity, &DynamicContentType, &DynamicContentMetadata)>,
    mut commands: Commands,
) {
    for (entity, type_, metadata) in query.iter() {
        if *type_ == DynamicContentType::BlogpostTagPage {
            continue;
        }
        let url = metadata_to_url(&config, metadata);
        commands.entity(entity).insert(url);
    }
}

struct UrlFor {
    config: Config,
}

impl tera::Function for UrlFor {
    fn call(&self, args: &HashMap<String, Value>) -> tera::Result<Value> {
        let route = match args.get("route") {
            Some(val) => tera::from_value::<String>(val.clone())
                .map_err(|_| tera::Error::msg("invalid route")),
            None => Err(tera::Error::msg("missing route")),
        }?;
        let url = url_for_impl(&self.config, &route, &args);
        Ok(tera::to_value(url.url)?)
    }
}

// A single entry to show in the navbar
#[derive(Clone, Debug, Serialize)]
struct NavbarEntry {
    url: String,
    title: String,
    active: bool,
}

// Top level navbar available on pages to show at the top
// Contains all top level routes that have a navbar enabled
#[derive(Debug, Serialize)]
struct Navbar {
    entries: Vec<NavbarEntry>,
}

impl Navbar {
    fn for_(&self, url: &str) -> Navbar {
        let entries: Vec<_> = self
            .entries
            .iter()
            .map(|e| {
                let active = e.url == url || (e.url != "/" && url.starts_with(&e.url));
                NavbarEntry {
                    active,
                    ..e.clone()
                }
            })
            .collect();
        Self { entries }
    }
}

fn navbar_indexer(query: Query<(&URL, &DynamicContentMetadata)>, mut commands: Commands) {
    let entries: Vec<_> = query
        .iter()
        .filter(|(_, metadata)| metadata.navbar.is_some())
        .sorted_by(|a, b| a.1.navbar.cmp(&b.1.navbar))
        .map(|(url, metadata)| NavbarEntry {
            url: url.url.to_string(),
            title: metadata.title.clone(),
            active: false,
        })
        .collect();
    commands.insert_resource(Navbar { entries });
}

// A single entry for a post in the blogpost index
#[derive(Clone, Debug, Serialize)]
struct BlogpostIndexEntry {
    url: String,
    slug: String,
    title: String,
    excerpt: String,
    date: String,
    year: String,
    month: String,
    month_name: String,
    day: String,
    tags: Vec<String>,
    featured: bool,
}

// Top level index available for all entries in the blog
// Contains all the posts and methods to access them efficiently
// All results are in reverse sorted order by date
#[derive(Clone, Debug, Serialize)]
struct BlogpostIndex {
    entries: Vec<BlogpostIndexEntry>,
}

impl BlogpostIndex {
    fn featured(&self) -> Vec<BlogpostIndexEntry> {
        self.entries
            .iter()
            .filter(|e| e.featured)
            .cloned()
            .collect()
    }

    fn recent(&self) -> Vec<BlogpostIndexEntry> {
        self.entries.clone()
    }

    fn tags_and_counts(&self) -> Vec<(String, usize)> {
        self.entries
            .iter()
            .flat_map(|e| e.tags.iter())
            .counts()
            .into_iter()
            .sorted_by(|a, b| match b.1.cmp(&a.1) {
                std::cmp::Ordering::Equal => b.0.cmp(a.0),
                o => o,
            })
            .map(|(s, c)| (s.clone(), c))
            .collect()
    }

    fn archives(&self) -> Vec<(String, String, Vec<BlogpostIndexEntry>)> {
        self.entries
            .iter()
            .map(|e| ((e.year.clone(), e.month_name.clone()), e.clone()))
            .into_group_map()
            .into_iter()
            .collect::<BTreeMap<_, _>>()
            .into_iter()
            .map(|((y, m), v)| (y, m, v))
            .rev()
            .collect()
    }
}

#[derive(Serialize)]
struct BlogpostTagsAndCounts {
    entries: Vec<(String, usize)>,
}

#[derive(Serialize)]
struct BlogpostArchives {
    entries: Vec<(String, String, Vec<BlogpostIndexEntry>)>,
}

fn blogpost_indexer(
    query: Query<(&DynamicContentType, &URL, &DynamicContentMetadata)>,
    mut commands: Commands,
) {
    let month_names = maplit::hashmap! {
        "01" => "January",
        "02" => "February",
        "03" => "March",
        "04" => "April",
        "05" => "May",
        "06" => "June",
        "07" => "July",
        "08" => "August",
        "09" => "September",
        "10" => "October",
        "11" => "November",
        "12" => "December",
    };
    let mut entries: Vec<_> = query
        .iter()
        .filter(|(type_, _, _)| **type_ == DynamicContentType::Blogpost)
        .map(|(_, url, metadata)| {
            let get_str = |s: &str| metadata.stuff.get(s).unwrap().as_str().unwrap().to_string();
            // This unwrap is safe, we create the slug
            let slug = get_str("slug");
            // These unwraps are safe as we do validation when creating the metadata
            let date = get_str("date");
            let year = get_str("year");
            let month = get_str("month");
            let day = get_str("day");
            // Do some basic validation
            let month_name = month_names
                .get(month.as_str())
                .expect(&format!("Invalid month: {} for URL {}", month, url.url))
                .to_string();
            // We need to validate the excerpt
            let excerpt = metadata
                .stuff
                .get("excerpt")
                .expect(&format!("No excerpt provided for blogpost at {}!", url.url))
                .as_str()
                .expect(&format!(
                    "Excerpt is not a string for blogpost at {}",
                    url.url
                ))
                .to_string();
            // We have a safe default
            let tags: Vec<String> = metadata
                .stuff
                .get("tags")
                .unwrap_or(&serde_json::Value::Array(vec![]))
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .filter_map(|e| e.as_str())
                .map(|s| s.to_string())
                .collect();
            // Safe default here too
            let featured = metadata
                .stuff
                .get("featured")
                .unwrap_or(&serde_json::Value::Bool(false))
                .as_bool()
                .unwrap_or(false);
            BlogpostIndexEntry {
                url: url.url.clone(),
                slug,
                title: metadata.title.clone(),
                excerpt,
                date,
                year,
                month,
                month_name,
                day,
                tags,
                featured,
            }
        })
        .collect();
    // Reverse compare
    entries.sort_by(|a, b| b.date.cmp(&a.date));
    commands.insert_resource(BlogpostIndex { entries });
}

#[derive(Serialize)]
struct Sitemap {
    entries: Vec<String>,
}

fn sitemap_indexer(query: Query<&URL, Without<IsStatiContent>>, mut commands: Commands) {
    let entries: BTreeSet<String> = query.iter().map(|u| u.url.clone()).collect();
    commands.insert_resource(Sitemap {
        entries: entries.into_iter().collect(),
    })
}

fn tag_page_generator(
    config: Res<Config>,
    index: Res<BlogpostIndex>,
    mut sitemap: ResMut<Sitemap>,
    query: Query<(
        &DynamicContentType,
        &DynamicContentMetadata,
        &RelativeSourcePath,
        &DynamicContentContents,
    )>,
    mut commands: Commands,
) {
    let tags: Vec<String> = index
        .tags_and_counts()
        .into_iter()
        .map(|(s, _)| s)
        .collect();
    for (type_, metadata, source_path, contents) in query.iter() {
        if *type_ != DynamicContentType::BlogpostTagPage {
            continue;
        }
        for tag in &tags {
            // TODO: See if we can avoid expensive copies
            let mut metadata = metadata.clone();
            metadata.stuff.insert("tag".to_string(), tag.clone().into());
            let url = metadata_to_url(&config, &metadata);
            sitemap.entries.push(url.url.clone());
            let source_path = source_path.clone();
            let contents = contents.clone();
            commands
                .spawn()
                .insert(source_path)
                .insert(IsDynamicContent {})
                .insert(metadata)
                .insert(contents)
                .insert(type_.clone())
                .insert(url);
        }
    }
    sitemap.entries.sort();
}

// TODO: See if there's a way to avoid copies
struct BlogpostFetcherFunction {
    entries: Vec<BlogpostIndexEntry>,
    skip_featured: bool,
}

impl tera::Function for BlogpostFetcherFunction {
    fn call(&self, args: &HashMap<String, Value>) -> tera::Result<Value> {
        let count = match args.get("count") {
            Some(val) => tera::from_value::<usize>(val.clone())
                .map_err(|_| tera::Error::msg("invalid count")),
            None => Err(tera::Error::msg("invalid count")),
        }?;

        let tag = match args.get("tag") {
            Some(val) => tera::from_value::<String>(val.clone())
                .map_err(|_| tera::Error::msg("invalid tag"))
                .map(|s| Some(s)),
            None => Ok(None),
        }?;

        Ok(Value::Array(
            self.entries
                .iter()
                .filter(|e| !self.skip_featured || !e.featured)
                .filter(|e| match tag.as_ref() {
                    Some(tag) => e.tags.contains(tag),
                    None => true,
                })
                .take(count)
                .filter_map(|e| tera::to_value(e).ok())
                .collect(),
        ))
    }

    fn is_safe(&self) -> bool {
        false
    }
}

fn dynamic_content_generator(
    config: Res<Config>,
    navbar: Res<Navbar>,
    blogindex: Res<BlogpostIndex>,
    sitemap: Res<Sitemap>,
    mut tera: ResMut<Tera>,
    query: Query<(
        Entity,
        &URL,
        &DynamicContentType,
        &DynamicContentMetadata,
        &DynamicContentContents,
    )>,
    mut commands: Commands,
) {
    // TODO: Move some of these to Tera filters
    let featured_posts = BlogpostFetcherFunction {
        entries: blogindex.featured(),
        skip_featured: false,
    };
    let recent_posts = BlogpostFetcherFunction {
        entries: blogindex.recent(),
        skip_featured: true,
    };
    let tagged_posts = BlogpostFetcherFunction {
        entries: blogindex.recent(),
        skip_featured: false,
    };
    let all_posts = BlogpostFetcherFunction {
        entries: blogindex.recent(),
        skip_featured: false,
    };
    tera.register_function("blogposts_featured", featured_posts);
    tera.register_function("blogposts_recent", recent_posts);
    tera.register_function("blogposts_tagged", tagged_posts);
    tera.register_function("blogposts_all", all_posts);
    let url_for = UrlFor {
        config: config.clone(),
    };
    tera.register_function("url_for", url_for);
    let tags_and_counts = tera::to_value(BlogpostTagsAndCounts {
        entries: blogindex.tags_and_counts(),
    })
    .expect("Couldn't serialize blogpost tags and counts!");
    let blog_archives = tera::to_value(BlogpostArchives {
        entries: blogindex.archives(),
    })
    .expect("Couldn't serialize blogpost archives");
    let sitemap = tera::to_value(Sitemap {
        entries: sitemap.entries.clone(),
    })
    .expect("Couldn't serialize sitemap");
    let blogpost_template_contents = std::fs::read_to_string(PathBuf::from(
        tera.get_template(&config.blogpost_template)
            .expect("Couldn't load blogpost template!")
            .path
            .clone()
            .expect("Blogpost template has no path!"),
    ))
    .expect(&format!(
        "Couldn't read blogpost template from {}!",
        &config.blogpost_template
    ));
    // TODO: Figure out parallelization
    for (entity, url, type_, metadata, contents) in query.iter() {
        let mut context = tera::Context::new();
        context.insert("sitename", &config.sitename);
        context.insert("title", &metadata.title);
        metadata
            .stuff
            .iter()
            .for_each(|(k, v)| context.insert(k, v));
        context.insert("navbar", &navbar.for_(&url.url));
        let html_output = if metadata.markdown {
            let parser = pulldown_cmark::Parser::new_ext(
                &contents.contents,
                pulldown_cmark::Options::empty(),
            );
            let mut html_output: String = String::with_capacity(contents.contents.len() * 3 / 2);
            pulldown_cmark::html::push_html(&mut html_output, parser);
            context.insert("content", &html_output);
            html_output
        } else {
            String::new()
        };
        context.insert("blog_tags_and_counts", &tags_and_counts);
        context.insert("blog_archives", &blog_archives);
        context.insert("sitemap", &sitemap);
        context.insert("url_for_this", &url.url);
        context.insert("og_url", &url.absolute);
        context.insert("og_type", &metadata.og_type);
        if !metadata.og_title.is_empty() {
            context.insert("og_title", &metadata.og_title);
        } else {
            context.insert("og_title", &metadata.title);
        }
        context.insert("og_description", &metadata.og_description);
        // TODO: Better error messages
        let contents = if let Some(template_name) = metadata.template.clone() {
            match type_ {
                DynamicContentType::Blogpost => {
                    // TODO: This should apply for all markdown stuff in the general case
                    // this is special cased once for blogposts to make the template loading by doing it once.
                    assert!(template_name == config.blogpost_template);
                    // For blogposts, replace in the markdown manually into the template
                    // and then render that template.
                    // This ensures that macro usage inside the markdown works
                    let template =
                        blogpost_template_contents.replace("{{ content | safe }}", &html_output);
                    tera.render_str(&template, &context)
                        .expect(&format!("Error generating source for {}", url.url))
                }
                _ => {
                    // render the template as is
                    tera.render(&template_name, &context)
                        .expect(&format!("Error generating source for {}", url.url))
                }
            }
        } else {
            tera.render_str(&contents.contents, &context)
                .expect(&format!("Error generating source for {}", url.url))
        };
        commands
            .entity(entity)
            .insert(WriteContentsToFile { contents });
    }
}

// An input source file loaded from somewhere
// Paths are relative to cwd
// TODO: Verify this!
#[derive(Debug, Clone)]
struct RelativeSourcePath {
    path: PathBuf,
}

// Location where this should be written out
// All relative paths should eventually be made absolute
#[derive(Debug)]
struct RelativeOutputPath {
    path: PathBuf,
}

fn map_urls_to_relative_paths(
    query: Query<(Entity, &URL), Without<RelativeOutputPath>>,
    mut commands: Commands,
) {
    for (entity, url) in query.iter() {
        let mut path = PathBuf::from(url.url.clone());
        if path.extension().is_none() {
            // We're only generating HTML for now so this is fine
            // Should probably be smarter in the future or enforce invariants
            path.push("index.html");
        }
        commands.entity(entity).insert(RelativeOutputPath { path });
    }
}

#[derive(Debug)]
struct AbsoluteOutputPath {
    path: PathBuf,
}

fn path_absoluter(
    config: Res<Config>,
    query: Query<(Entity, &RelativeOutputPath)>,
    mut commands: Commands,
) {
    for (entity, source) in query.iter() {
        let path = if source.path.is_absolute() {
            source.path.strip_prefix("/").expect("Stripping failed!")
        } else {
            source.path.as_path()
        };
        commands.entity(entity).insert(AbsoluteOutputPath {
            path: config.output_dir.join(path),
        });
    }
}

// Static file copy
struct IsStatiContent {}
struct CopySourceToOutput {}

fn output_folder_creator(query: Query<&AbsoluteOutputPath>) {
    let paths: HashSet<_> = query
        .iter()
        .filter_map(|p| {
            let path = p.path.as_path();
            if path.is_dir() {
                Some(path)
            } else {
                path.parent()
            }
        })
        .collect();
    for path in paths {
        std::fs::create_dir_all(path).expect(&format!(
            "Could not create directory: {}",
            path.to_string_lossy()
        ));
    }
}

fn static_file_copier(
    pool: Res<ComputeTaskPool>,
    query: Query<(
        &RelativeSourcePath,
        &AbsoluteOutputPath,
        &CopySourceToOutput,
    )>,
) {
    // TODO: Look at batch sizes here
    query.par_for_each(&pool, 8, |(from, to, _)| {
        std::fs::copy(from.path.as_path(), to.path.as_path()).expect(&format!(
            "Unable to copy {} to {}",
            from.path.as_path().to_string_lossy(),
            to.path.as_path().to_string_lossy()
        ));
    });
}

struct WriteContentsToFile {
    contents: String,
}

fn file_contents_writer(
    pool: Res<ComputeTaskPool>,
    query: Query<(&AbsoluteOutputPath, &WriteContentsToFile)>,
) {
    // TODO: Look at batch sizes here
    query.par_for_each(&pool, 8, |(path, contents)| {
        std::fs::write(path.path.as_path(), &contents.contents).expect(&format!(
            "Unable to write output to {}",
            path.path.as_path().to_string_lossy()
        ));
    });
}

#[derive(Debug, Hash, PartialEq, Eq, Clone, StageLabel)]
enum Stage {
    // Process the configs, create the loaders
    ConfigProcessing,
    // Load all the sources into memory as appropriate
    SourceLoading,
    // Analyzing dynamic content, generates items from each content item
    AnalyzingDynamicContent,
    // Indexes dynamic content, looking up things from the previous analysis
    IndexingDynamicContent,
    // Spawning more (derived) dynamic content based on index results
    SpawningDynamicContent,
    // Generating dynamic content
    GeneratingDynamicContent,
    // Preparing output for persistence
    PreparingForPersistence,
    // Final stage. Write out all the output
    PersistOutput,
}

#[derive(Debug, Hash, PartialEq, Eq, Clone, SystemLabel)]
enum SystemTag {
    OutputFolderCreator,
}

fn run(config: Config) {
    App::build()
        .insert_resource(config)
        .add_stage_before(
            CoreStage::Update,
            Stage::ConfigProcessing,
            SystemStage::parallel(),
        )
        .add_system_to_stage(Stage::ConfigProcessing, create_source_loaders.system())
        .add_stage_after(
            Stage::ConfigProcessing,
            Stage::SourceLoading,
            SystemStage::parallel(),
        )
        .add_system_to_stage(Stage::SourceLoading, static_content_source_loader.system())
        .add_system_to_stage(Stage::SourceLoading, template_source_loader.system())
        .add_system_to_stage(Stage::SourceLoading, dynamic_content_source_loader.system())
        .add_stage_after(
            Stage::SourceLoading,
            Stage::AnalyzingDynamicContent,
            SystemStage::parallel(),
        )
        .add_system_to_stage(Stage::AnalyzingDynamicContent, generate_urls.system())
        .add_stage_after(
            Stage::AnalyzingDynamicContent,
            Stage::IndexingDynamicContent,
            SystemStage::parallel(),
        )
        .add_system_to_stage(Stage::IndexingDynamicContent, navbar_indexer.system())
        .add_system_to_stage(Stage::IndexingDynamicContent, blogpost_indexer.system())
        .add_system_to_stage(Stage::IndexingDynamicContent, sitemap_indexer.system())
        .add_stage_after(
            Stage::IndexingDynamicContent,
            Stage::SpawningDynamicContent,
            SystemStage::parallel(),
        )
        .add_system_to_stage(Stage::SpawningDynamicContent, tag_page_generator.system())
        .add_stage_after(
            Stage::SpawningDynamicContent,
            Stage::GeneratingDynamicContent,
            SystemStage::parallel(),
        )
        .add_system_to_stage(
            Stage::GeneratingDynamicContent,
            map_urls_to_relative_paths.system(),
        )
        .add_system_to_stage(
            Stage::GeneratingDynamicContent,
            dynamic_content_generator.system(),
        )
        .add_stage_after(
            Stage::GeneratingDynamicContent,
            Stage::PreparingForPersistence,
            SystemStage::parallel(),
        )
        .add_system_to_stage(Stage::PreparingForPersistence, path_absoluter.system())
        .add_stage_after(
            Stage::PreparingForPersistence,
            Stage::PersistOutput,
            SystemStage::parallel(),
        )
        .add_system_to_stage(
            Stage::PersistOutput,
            output_folder_creator
                .system()
                .label(SystemTag::OutputFolderCreator),
        )
        .add_system_to_stage(
            Stage::PersistOutput,
            static_file_copier
                .system()
                .after(SystemTag::OutputFolderCreator),
        )
        .add_system_to_stage(
            Stage::PersistOutput,
            file_contents_writer
                .system()
                .after(SystemTag::OutputFolderCreator),
        )
        .run();
}

#[derive(Debug, StructOpt)]
#[structopt(name = "suji", about = "Static site generator.")]
struct Args {
    #[structopt(help = "Path to config file")]
    config_path: String,

    #[structopt(long, help = "Whether to watch for changes.")]
    watch: bool,

    #[structopt(long, help = "Whether to serve output directory.")]
    serve: bool,

    #[structopt(long, help = "Port to bind.", default_value = "8000")]
    port: u16,
}

#[tokio::main]
async fn main() {
    let args = Args::from_args();
    let source = std::fs::read_to_string(&args.config_path)
        .expect(&format!("Unable to config file {}", args.config_path));
    let mut config: Config =
        serde_json::from_str(&source).expect("Config is not in the expected format!");
    let cwd = std::env::current_dir().expect("Couldn't get current dir!");

    // TODO: Verify config is inside source dir
    if config.source_dir.to_string_lossy() == "." {
        config.source_dir = cwd.clone();
    }
    if config.source_dir.is_relative() {
        config.source_dir = cwd.join(config.source_dir);
    }
    if config.output_dir.is_relative() {
        config.output_dir = cwd.join(config.output_dir);
    }

    let decorator = slog_term::TermDecorator::new().build();
    let drain = slog_term::FullFormat::new(decorator).build().fuse();
    let drain = slog_async::Async::new(drain).build().fuse();

    let logger = slog::Logger::root(drain, o!());

    info!(logger, "Running initial generation...");
    run(config.clone());

    if args.watch {
        let config = config.clone();
        let source_dir = config.source_dir.clone();
        let output_dir = config.output_dir.clone();
        let logger = logger.clone();
        tokio::task::spawn_blocking(move || {
            let logger2 = logger.clone();
            let mut watcher = Hotwatch::new().expect("Couldn't create watcher!");
            watcher
                    .watch(&source_dir, move |event| {
                        if let Event::Error(e, path) = event {
                            error!(logger2, "Error watching file system!"; "error" => ?e, "path" => ?path);
                            Flow::Exit
                        } else {
                            let maybe_path = match &event {
                                // These events can't affect the output
                                Event::Chmod(_) | Event::Error(_, _) | Event::Rescan => None,
                                // We ignore the Write and Remove as we want to react faster,
                                // so we just handle the notices
                                Event::Write(_) | Event::Remove(_) => None,
                                Event::Create(path) => Some(path),
                                Event::NoticeWrite(path) | Event::NoticeRemove(path) => Some(path),
                                Event::Rename(_from, to) => Some(to),
                            };
                            if let Some(path) = maybe_path {
                                if path.starts_with(&output_dir) {
                                    // Swallow
                                } else {
                                    info!(logger2, "Rerunning generation..."; "event" => ?event);
                                    run(config.clone());
                                }
                            };
                            Flow::Continue
                        }
                    })
                    .expect("Couldn't watch!");
            info!(logger, "Watcher successfully set up...");
            watcher.run();
        });
    }

    if args.serve {
        let logger2 = logger.clone();
        let app = Router::new().nest(
            "/",
            service::get(ServeDir::new(config.output_dir.clone())).handle_error(
                move |error: std::io::Error| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        error!(logger2, "Unhandled internal error"; "error" => ?error),
                    )
                },
            ),
        );
        let addr = SocketAddr::from(([127, 0, 0, 1], args.port));
        info!(logger, "Setup HTTP server to listen on"; "port" => args.port);
        axum::Server::bind(&addr)
            .serve(app.into_make_service())
            .await
            .expect("Couldn't serve output directory!")
    }
}
