Suji - A Static Site Generator
=================================

Suji is a single file static site generator written in Rust. It's a light-weight single binary you can run, with minimal compile time dependencies.

Suji is the successor to [Halwa](https://github.com/mhlakhani/halwa) (for the etymology nerds, suji is an orange halwa - because Rust is orange. sorry. not sorry.)

# Usage

Using Suji is as easy as it should be; build it, then create a configuration file and point Suji to it:

> git checkout ...
> cargo build
> cd $folder_with_contents
> /path/to/suji config.json --serve

This will generate an output website and serve it on localhost port 8000.

You can specify `--watch` and it will automatically regenerate everything if an input file is changed.

A sample configuration is available at [https://github.com/mhlakhani/mhlakhani-com](https://github.com/mhlakhani/mhlakhani-com).

# How it works

(warning: mini blog post ahead)

Suji is an experiment in using [ECS](https://en.wikipedia.org/wiki/Entity_component_system) to generate a blog. If you look at the README for Halwa, it was basically inexperienced me trying to reinvent something like an ECS without knowing what that was.

In Suji, (almost) *everything* works inside the ECS model. All pages, blogposts or whatever, are treated as entities and processed in various stages. There's still a bunch of things which are currently hard-coded to meet my own blog's needs that I hope to generalize in the future.

Unlike Halwa, there is no caching support. It just runs fast enough that I haven't needed to add support for it yet - though this might change in the future as a learning exercise.

The rest of this README explains how it works and why ECS is a good fit. We assume the reader is familiar with ECS. If not, [this is a good intro](https://bevy-cheatbook.github.io/programming.html)

## Configuration

A config file orchestrates everything. It's fairly straightforward - beyond some metadata (like filepaths) the main things specified are:

* A list of routes (e.g. the `publications` route is at `/publications/`)
* A map from filepaths to content types.

Based on that, each type of content gets generated. We support content types like the following (full list in `SourceType`):

* `StaticContent` (just copy file from the source to the destination path)
* `Template` (load this file as a `Template` for the Tera templating engine)
* `Sitemap` (Load this as a sitemap)
* `DynamicContentSinglePage` (single page file which can have dynamic elements)
* `DynamicContentBlogPost` (single page file treated as a blog post)
* .... you get the idea

## The pipeline

We define a number of stages, each comprised of (potentially) multiple systems. While stages are run one by one, the systems within run in parallel. The stages are self explanatory:

* `ConfigProcessing`: Load the config, create loaders to load the data
* `SourceLoadingStage`: Load each source file into the ECS, creating entities
* `AnalyzingDynamicContentStage`: Generate URLs as needed
* `IndexingDynamicContentStage`: Index all the content, creating navbars, sitemaps, etc
* `SpawningDynamicContentStage`: Dynamically spawn new DynamicContent entities (for tag pages)
* `GeneratingDynamicContentStage`: Render markdown/dynamic pages to static HTML
* `PreparingForPersistenceStage`: Prepare the data for writing to disk (generating absolute paths, etc)
* `PersistOutputStage`: Create output folders, copy static files, write HTML files (all in parallel)

## The components and entities

We (ab)use ECS for almost everything. Everything is an entity or a component, so we can easily handle everything with Bevy. A few examples:

When we load the config, we create one entity for each source we need to load. Then we have a system that goes over these and creates follow-on entities.
For example, `static_content_source_loader` will iterate over all the files in a glob, and create entities with the following components:

* `RelativeSourcePath` (where the content came from)
* `URL` (URL it's located at)
* `RelativeOutputPath` - to mark that the final output path is relative to the source path
* `CopySourcePath` - marker component to copy the file as is
* `IsStaticContent` - marker component
* `ExcludeFromSitemap` - marker component

We similarly have entities to handle dynamic content, blog posts, the navbar entries, etc. 

# So, was it worth it?

Well, this was a learning exercise mostly, so the answer is trivially "yes". More seriously, I think this highlighted that ECS and data driven design is a fairly good approach for creating websites and/or static sites. A few benefits relaly stood out:

* It's surprisingly performant: This isn't just because I use Rust, but because we can trivially parallelize processing of independent entities.
* It's easy to extend: Adding new content types is trivial - just add a new entity and a few systems to handle it.

Some things don't work super well yet though, especially things like parent/child relationships -- I had to hack things together. The newer versions of `Bevy` have better support for this, though, and I need to try it out.