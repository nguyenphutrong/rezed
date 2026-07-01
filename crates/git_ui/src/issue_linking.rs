use std::{ops::Range, sync::Arc};

use git::{GitHostingProviderRegistry, parse_git_remote_url, repository::RepoPath};
use gpui::{AnyElement, App, Context, Entity, IntoElement, ParentElement, SharedString, Styled};
use markdown::{
    Markdown,
    parser::{MarkdownEvent, MarkdownTag, parse_links_only},
};
use project::{
    git_store::Repository,
    project_settings::{IssueLinkingRule, ProjectSettings},
};
use regex::Regex;
use settings::Settings;
use ui::{ButtonLike, ButtonSize, Color, HighlightedLabel, Label, LabelSize, prelude::*};
use util::rel_path::RelPath;

#[derive(Clone, Default)]
pub(crate) struct IssueLinkingRules {
    rules: Arc<[CompiledIssueLinkingRule]>,
}

#[derive(Clone)]
struct CompiledIssueLinkingRule {
    regex: Regex,
    issue_url: String,
}

#[derive(Default)]
struct IssueUrlVariables {
    git_remote_url: Option<String>,
    git_remote_host: Option<String>,
    git_remote_owner: Option<String>,
    git_remote_repo: Option<String>,
    git_provider: Option<String>,
    git_provider_url: Option<String>,
    github_host: Option<String>,
    github_base_url: Option<String>,
    github_org: Option<String>,
    github_repo: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IssueLink {
    pub range: Range<usize>,
    pub url: SharedString,
    kind: IssueLinkKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IssueLinkKind {
    RawUrl,
    Issue,
}

impl IssueLinkingRules {
    #[cfg(test)]
    pub(crate) fn new(rules: &[IssueLinkingRule]) -> Self {
        Self::new_with_variables(rules, &IssueUrlVariables::default())
    }

    fn new_with_variables(rules: &[IssueLinkingRule], variables: &IssueUrlVariables) -> Self {
        let rules = rules
            .iter()
            .filter(|rule| rule.enabled)
            .filter_map(|rule| {
                Some(CompiledIssueLinkingRule {
                    regex: Regex::new(&rule.issue_regex).ok()?,
                    issue_url: variables.expand_template(&rule.issue_url)?,
                })
            })
            .collect::<Vec<_>>();

        Self {
            rules: Arc::from(rules),
        }
    }

    pub(crate) fn for_repository(repository: &Entity<Repository>, cx: &App) -> Self {
        let (project_path, remote_url) = {
            let repository = repository.read(cx);
            (
                repository
                    .repo_path_to_project_path(&RepoPath::from_rel_path(RelPath::empty()), cx),
                repository.default_remote_url(),
            )
        };

        let rules = if let Some(project_path) = project_path.as_ref() {
            ProjectSettings::get(Some(project_path.into()), cx)
                .git
                .issue_linking
                .clone()
        } else {
            ProjectSettings::get(None, cx).git.issue_linking.clone()
        };

        let variables = IssueUrlVariables::from_remote_url(remote_url.as_deref(), cx);
        Self::new_with_variables(&rules, &variables)
    }

    pub(crate) fn markdown(&self, source: SharedString, cx: &mut Context<Markdown>) -> Markdown {
        if let Some(markdown_source) = self.linked_markdown_source(source.as_ref()) {
            Markdown::new(markdown_source.into(), None, None, cx)
        } else {
            Markdown::new_text(source, cx)
        }
    }

    pub(crate) fn render_label(
        &self,
        text: SharedString,
        label_size: LabelSize,
        color: Color,
        truncate: bool,
        highlight_ranges: Vec<Range<usize>>,
    ) -> AnyElement {
        let issue_links = self.issue_links_for_text(text.as_ref());
        if issue_links.is_empty() {
            return render_text_segment(
                text,
                0..usize::MAX,
                &highlight_ranges,
                label_size,
                color,
                false,
                truncate,
            );
        }

        let mut elements = Vec::new();
        let mut start = 0;

        for (ix, link) in issue_links.into_iter().enumerate() {
            if start < link.range.start {
                elements.push(render_text_segment(
                    text[start..link.range.start].to_string().into(),
                    start..link.range.start,
                    &highlight_ranges,
                    label_size,
                    color,
                    false,
                    truncate,
                ));
            }

            let link_text = text[link.range.clone()].to_string();
            let url = link.url.to_string();
            elements.push(
                ButtonLike::new(format!(
                    "issue-link-{ix}-{}-{}",
                    link.range.start, link.range.end
                ))
                .size(ButtonSize::None)
                .child(render_text_segment(
                    link_text.into(),
                    link.range.clone(),
                    &highlight_ranges,
                    label_size,
                    color,
                    true,
                    truncate,
                ))
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    cx.open_url(&url);
                })
                .into_any_element(),
            );

            start = link.range.end;
        }

        if start < text.len() {
            elements.push(render_text_segment(
                text[start..].to_string().into(),
                start..text.len(),
                &highlight_ranges,
                label_size,
                color,
                false,
                truncate,
            ));
        }

        h_flex()
            .min_w_0()
            .overflow_hidden()
            .children(elements)
            .into_any_element()
    }

    pub(crate) fn issue_links_for_text(&self, text: &str) -> Vec<IssueLink> {
        self.links_for_text(text)
            .into_iter()
            .filter(|link| link.kind == IssueLinkKind::Issue)
            .collect()
    }

    pub(crate) fn linked_markdown_source(&self, text: &str) -> Option<String> {
        let links = self.links_for_text(text);
        if !links.iter().any(|link| link.kind == IssueLinkKind::Issue) {
            return None;
        }

        let mut markdown = String::new();
        let mut start = 0;

        for link in links {
            markdown.push_str(&Markdown::escape(&text[start..link.range.start]));
            markdown.push('[');
            markdown.push_str(&Markdown::escape(&text[link.range.clone()]));
            markdown.push_str("](<");
            markdown.push_str(link.url.as_ref());
            markdown.push_str(">)");
            start = link.range.end;
        }

        markdown.push_str(&Markdown::escape(&text[start..]));
        Some(markdown)
    }

    fn links_for_text(&self, text: &str) -> Vec<IssueLink> {
        let mut links = raw_url_links(text);
        let mut claimed_ranges = links
            .iter()
            .map(|link| link.range.clone())
            .collect::<Vec<_>>();

        for rule in self.rules.iter() {
            for captures in rule.regex.captures_iter(text) {
                let Some(issue_match) = captures.get(0) else {
                    continue;
                };
                let range = issue_match.range();
                if range.is_empty()
                    || claimed_ranges
                        .iter()
                        .any(|claimed| ranges_overlap(claimed, &range))
                {
                    continue;
                }

                let mut url = String::new();
                captures.expand(&rule.issue_url, &mut url);
                let Ok(url) = url::Url::parse(&url) else {
                    continue;
                };

                claimed_ranges.push(range.clone());
                links.push(IssueLink {
                    range,
                    url: url.to_string().into(),
                    kind: IssueLinkKind::Issue,
                });
            }
        }

        links.sort_by_key(|link| link.range.start);
        links
    }
}

impl IssueUrlVariables {
    fn from_remote_url(remote_url: Option<&str>, cx: &App) -> Self {
        let Some(remote_url) = remote_url else {
            return Self::default();
        };

        let mut variables = Self {
            git_remote_url: Some(remote_url.to_string()),
            git_remote_host: remote_host(remote_url),
            ..Default::default()
        };

        let Some(provider_registry) = GitHostingProviderRegistry::try_global(cx) else {
            return variables;
        };

        if let Some((provider, parsed_remote)) = parse_git_remote_url(provider_registry, remote_url)
        {
            let provider_name = provider.name();
            let provider_url = provider.base_url();
            variables.git_provider = Some(provider_name.clone());
            variables.git_provider_url =
                Some(provider_url.as_str().trim_end_matches('/').to_string());
            variables.git_remote_owner = Some(parsed_remote.owner.to_string());
            variables.git_remote_repo = Some(parsed_remote.repo.to_string());

            if provider_name.starts_with("GitHub") {
                variables.github_host = provider_url.host_str().map(str::to_string);
                variables.github_base_url =
                    Some(provider_url.as_str().trim_end_matches('/').to_string());
                variables.github_org = Some(parsed_remote.owner.to_string());
                variables.github_repo = Some(parsed_remote.repo.to_string());
            }
        }

        variables
    }

    fn expand_template(&self, template: &str) -> Option<String> {
        let mut expanded = template.to_string();

        for (name, value) in [
            ("GIT_REMOTE_URL", self.git_remote_url.as_deref()),
            ("GIT_REMOTE_HOST", self.git_remote_host.as_deref()),
            ("GIT_REMOTE_OWNER", self.git_remote_owner.as_deref()),
            ("GIT_REMOTE_REPO", self.git_remote_repo.as_deref()),
            ("GIT_PROVIDER_URL", self.git_provider_url.as_deref()),
            ("GIT_PROVIDER", self.git_provider.as_deref()),
            ("GITHUB_HOST", self.github_host.as_deref()),
            ("GITHUB_BASE_URL", self.github_base_url.as_deref()),
            ("GITHUB_ORG", self.github_org.as_deref()),
            ("GITHUB_REPO", self.github_repo.as_deref()),
        ] {
            let braced_variable = format!("${{{name}}}");
            let unbraced_variable = format!("${name}");
            if expanded.contains(&braced_variable) || expanded.contains(&unbraced_variable) {
                let value = value?;
                expanded = expanded
                    .replace(&braced_variable, value)
                    .replace(&unbraced_variable, value);
            }
        }

        Some(expanded)
    }
}

fn remote_host(remote_url: &str) -> Option<String> {
    if let Some(remote_url) = remote_url.strip_prefix("git@")
        && let Some((host, _)) = remote_url.split_once(':')
    {
        return Some(host.to_string());
    }

    url::Url::parse(remote_url)
        .ok()
        .and_then(|remote_url| remote_url.host_str().map(str::to_string))
}

fn raw_url_links(text: &str) -> Vec<IssueLink> {
    parse_links_only(text)
        .into_iter()
        .filter_map(|(range, event)| {
            let MarkdownEvent::Start(MarkdownTag::Link { dest_url, .. }) = event else {
                return None;
            };

            Some(IssueLink {
                range,
                url: dest_url,
                kind: IssueLinkKind::RawUrl,
            })
        })
        .collect()
}

fn ranges_overlap(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

fn render_text_segment(
    text: SharedString,
    source_range: Range<usize>,
    highlight_ranges: &[Range<usize>],
    label_size: LabelSize,
    color: Color,
    underline: bool,
    truncate: bool,
) -> AnyElement {
    let segment_highlights = highlight_ranges
        .iter()
        .filter_map(|highlight| {
            let start = highlight.start.max(source_range.start);
            let end = highlight.end.min(source_range.end);
            (start < end).then(|| start - source_range.start..end - source_range.start)
        })
        .collect::<Vec<_>>();

    if segment_highlights.is_empty() {
        let label = Label::new(text).size(label_size).color(color);
        let label = if underline { label.underline() } else { label };
        let label = if truncate { label.truncate() } else { label };
        label.into_any_element()
    } else {
        let label = HighlightedLabel::from_ranges(text, segment_highlights)
            .size(label_size)
            .color(color);
        let label = if underline { label.underline() } else { label };
        let label = if truncate { label.truncate() } else { label };
        label.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use markdown::{MarkdownElement, MarkdownStyle};

    use super::*;

    fn rule(name: &str, issue_regex: &str, issue_url: &str) -> IssueLinkingRule {
        IssueLinkingRule {
            name: Some(name.to_string()),
            issue_regex: issue_regex.to_string(),
            issue_url: issue_url.to_string(),
            enabled: true,
        }
    }

    fn github_variables() -> IssueUrlVariables {
        IssueUrlVariables {
            git_remote_url: Some("git@github.com:zed-industries/zed.git".to_string()),
            git_remote_host: Some("github.com".to_string()),
            git_remote_owner: Some("zed-industries".to_string()),
            git_remote_repo: Some("zed".to_string()),
            git_provider: Some("GitHub".to_string()),
            git_provider_url: Some("https://github.com".to_string()),
            github_host: Some("github.com".to_string()),
            github_base_url: Some("https://github.com".to_string()),
            github_org: Some("zed-industries".to_string()),
            github_repo: Some("zed".to_string()),
        }
    }

    #[test]
    fn test_multiple_issue_linking_rules() {
        let rules = IssueLinkingRules::new(&[
            rule("github", r"#(\d+)", "https://github.com/org/repo/issues/$1"),
            rule(
                "linear",
                r"(LIN-\d+)",
                "https://linear.app/company/issue/$1",
            ),
            rule(
                "jira",
                r"([A-Z][A-Z0-9]+-\d+)",
                "https://company.atlassian.net/browse/$1",
            ),
        ]);

        let links = rules.issue_links_for_text("Fix #123, LIN-456, and JIRA-789");
        assert_eq!(links.len(), 3);
        assert_eq!(links[0].range, 4..8);
        assert_eq!(
            links[0].url.as_ref(),
            "https://github.com/org/repo/issues/123"
        );
        assert_eq!(links[1].range, 10..17);
        assert_eq!(
            links[1].url.as_ref(),
            "https://linear.app/company/issue/LIN-456"
        );
        assert_eq!(links[2].range, 23..31);
        assert_eq!(
            links[2].url.as_ref(),
            "https://company.atlassian.net/browse/JIRA-789"
        );
    }

    #[test]
    fn test_first_issue_linking_rule_claims_overlapping_range() {
        let rules = IssueLinkingRules::new(&[
            rule(
                "jira",
                r"([A-Z][A-Z0-9]+-\d+)",
                "https://jira.test/browse/$1",
            ),
            rule("linear", r"([A-Z]+-\d+)", "https://linear.test/issue/$1"),
        ]);

        let links = rules.issue_links_for_text("Fix ABC-123");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url.as_ref(), "https://jira.test/browse/ABC-123");
    }

    #[test]
    fn test_invalid_rules_and_urls_are_skipped() {
        let rules = IssueLinkingRules::new(&[
            rule("invalid-regex", r"([", "https://invalid.test/$1"),
            rule("invalid-url", r"(BAD-\d+)", "not a url/$1"),
            IssueLinkingRule {
                enabled: false,
                ..rule("disabled", r"(OFF-\d+)", "https://disabled.test/$1")
            },
            rule("valid", r"(OK-\d+)", "https://valid.test/$1"),
        ]);

        let links = rules.issue_links_for_text("BAD-1 OFF-2 OK-3");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].range, 12..16);
        assert_eq!(links[0].url.as_ref(), "https://valid.test/OK-3");
    }

    #[test]
    fn test_issue_url_expands_repository_variables_before_regex_captures() {
        let variables = github_variables();
        let rules = IssueLinkingRules::new_with_variables(
            &[rule(
                "github",
                r"#(\d+)",
                "$GITHUB_BASE_URL/$GITHUB_ORG/$GITHUB_REPO/issues/$1",
            )],
            &variables,
        );

        let links = rules.issue_links_for_text("Fix #123");
        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0].url.as_ref(),
            "https://github.com/zed-industries/zed/issues/123"
        );
    }

    #[test]
    fn test_issue_url_expands_variables_with_shared_prefixes() {
        let variables = github_variables();
        let rules = IssueLinkingRules::new_with_variables(
            &[rule(
                "provider",
                r"#(\d+)",
                "https://links.test/?provider=$GIT_PROVIDER&base=$GIT_PROVIDER_URL&issue=$1",
            )],
            &variables,
        );

        let links = rules.issue_links_for_text("Fix #123");
        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0].url.as_ref(),
            "https://links.test/?provider=GitHub&base=https://github.com&issue=123"
        );
    }

    #[test]
    fn test_issue_url_skips_rules_with_missing_repository_variables() {
        let rules = IssueLinkingRules::new_with_variables(
            &[rule(
                "github",
                r"#(\d+)",
                "https://github.com/$GITHUB_ORG/$GITHUB_REPO/issues/$1",
            )],
            &IssueUrlVariables::default(),
        );

        assert!(rules.issue_links_for_text("Fix #123").is_empty());
    }

    #[test]
    fn test_issue_links_do_not_override_raw_urls() {
        let rules =
            IssueLinkingRules::new(&[rule("linear", r"(ABC-\d+)", "https://linear.test/issue/$1")]);

        let links = rules.issue_links_for_text("See https://example.test/ABC-1 and ABC-1");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].range, 35..40);
    }

    #[gpui::test]
    fn test_issue_linked_markdown_preserves_rendered_text(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });

        let rules =
            IssueLinkingRules::new(&[rule("linear", r"(ABC-\d+)", "https://linear.test/issue/$1")]);
        let source = "Fix *ABC-123* and keep https://example.test/ABC-999";
        let markdown = cx.new(|cx| rules.markdown(source.into(), cx));

        cx.run_until_parked();
        let cx = cx.add_empty_window();

        let rendered =
            MarkdownElement::rendered_text(markdown, cx, |_, _| MarkdownStyle::default());
        assert_eq!(rendered, source);
    }

    #[test]
    fn test_remote_host_parses_https_and_ssh_remotes() {
        assert_eq!(
            remote_host("https://github.com/zed-industries/zed.git").as_deref(),
            Some("github.com")
        );
        assert_eq!(
            remote_host("git@github.com:zed-industries/zed.git").as_deref(),
            Some("github.com")
        );
    }
}
