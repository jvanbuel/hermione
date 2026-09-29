//! Courses: listing, creating, the settings panel, and who teaches them.

use super::scope::authorized_course;
use crate::auth::AuthCtx;
use crate::state::AppState;
use crate::tenancy;
use axum::extract::Extension;
use axum::extract::Path;
use axum::extract::Query;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::Json;
use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CourseDto {
    slug: String,
    name: String,
    repo_url: Option<String>,
    archived: bool,
    /// Included so the switcher can show it as a tooltip.
    description: Option<String>,
    /// Whether reference solutions are set up, so the file pane knows whether to
    /// offer the Solution view. Only the fact — never any solution text.
    has_solutions: bool,
}

impl From<hermione_entity::courses::Model> for CourseDto {
    fn from(c: hermione_entity::courses::Model) -> Self {
        let has_solutions = crate::solutions::SolutionsSource::from_course(&c).is_some();
        CourseDto {
            slug: c.slug,
            name: c.name,
            repo_url: c.repo_url,
            archived: c.archived_at.is_some(),
            description: c.description,
            has_solutions,
        }
    }
}

/// `?archived=1` (or `true`) lists archived courses instead of active ones.
#[derive(Deserialize)]
pub(super) struct ListCoursesQuery {
    archived: Option<String>,
}

/// Courses the caller may see (all of them in open dev mode). Active by default;
/// `?archived=1` returns archived ones (for the restore UI).
pub(super) async fn list_courses(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Query(q): Query<ListCoursesQuery>,
) -> Response {
    let archived = matches!(q.archived.as_deref(), Some("1" | "true" | "yes"));
    let courses = match ctx {
        AuthCtx::OpenDev => tenancy::all_courses(&state.db, archived).await,
        AuthCtx::Admin(admin_id) => tenancy::courses_for_admin(&state.db, admin_id, archived).await,
    };
    match courses {
        Ok(rows) => {
            let dtos: Vec<CourseDto> = rows.into_iter().map(CourseDto::from).collect();
            Json(dtos).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Normalizes a course slug to a URL-safe form (`[a-z0-9-]`), collapsing runs of
/// other characters into single dashes. Returns `None` if nothing usable remains.
pub(crate) fn normalize_slug(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !out.is_empty() && !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let slug = out.trim_matches('-').to_string();
    (!slug.is_empty()).then_some(slug)
}

/// The repository's short name — the last path segment of a git URL, minus any
/// `.git` suffix. Understands both `https://host/owner/name.git` and
/// `git@host:owner/name.git` forms. Used to derive a default slug/name.
fn repo_short_name(repo_url: &str) -> Option<String> {
    let trimmed = repo_url.trim().trim_end_matches('/');
    let tail = trimmed.rsplit(['/', ':']).next()?;
    let name = tail.strip_suffix(".git").unwrap_or(tail).trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Turns a slug into a human-friendly title ("intro-python" → "Intro Python").
pub(crate) fn title_from_slug(slug: &str) -> String {
    slug.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CreateCourseBody {
    slug: Option<String>,
    name: Option<String>,
    repo_url: Option<String>,
    /// Seed the course's exercises from the linked repo's folders (GitHub only).
    /// Defaults to true when a GitHub repo is linked.
    seed_exercises: Option<bool>,
    // Optional profile fields, set at creation.
    description: Option<String>,
    term: Option<String>,
    institution: Option<String>,
    level: Option<String>,
}

/// Trims a create-body field; empty ⇒ `None`.
pub(super) fn trimmed(v: &Option<String>) -> Option<String> {
    v.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The profile fields from a create body.
fn create_profile(body: &CreateCourseBody) -> tenancy::CourseProfile {
    tenancy::CourseProfile {
        description: trimmed(&body.description),
        term: trimmed(&body.term),
        institution: trimmed(&body.institution),
        level: trimmed(&body.level),
    }
}

/// The resolved (slug, name, repo_url) for a new course, or a client error
/// describing what's missing/invalid. A repo URL alone is enough — the slug and
/// name are derived from it — which is what "link a repo as a course" means.
fn resolve_new_course(body: &CreateCourseBody) -> Result<(String, String, Option<String>), String> {
    let repo_url = body
        .repo_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // Slug precedence: an explicit slug, else the repo's short name, else the
    // course name — so any one of the three fields is enough to create a course.
    let slug = match body
        .slug
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(explicit) => normalize_slug(explicit)
            .ok_or_else(|| "slug must contain a letter or digit".to_string())?,
        None => repo_url
            .as_deref()
            .and_then(repo_short_name)
            .and_then(|n| normalize_slug(&n))
            .or_else(|| body.name.as_deref().and_then(normalize_slug))
            .ok_or_else(|| "a name, slug, or repo URL is required".to_string())?,
    };

    let name = body
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| title_from_slug(&slug));

    Ok((slug, name, repo_url))
}

/// POST /api/courses — a signed-in teacher creates a course (optionally linked to
/// a git repo) and is automatically granted membership, so it appears in their
/// switcher right away.
pub(super) async fn create_course_for_teacher(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Json(body): Json<CreateCourseBody>,
) -> Response {
    let (slug, name, repo_url) = match resolve_new_course(&body) {
        Ok(parts) => parts,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };

    if tenancy::course_by_slug(&state.db, &slug).await.is_some() {
        return (
            StatusCode::CONFLICT,
            format!("a course with slug '{slug}' already exists"),
        )
            .into_response();
    }

    // Create the course with its profile in one INSERT — never a course without
    // the requested profile.
    let course = match tenancy::create_course_with_profile(
        &state.db,
        &slug,
        &name,
        repo_url.as_deref(),
        &create_profile(&body),
    )
    .await
    {
        Ok(course) => course,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };

    // The creating teacher becomes a member; open-dev callers aren't a specific
    // admin, so there's nobody to grant (they can already see every course).
    if let AuthCtx::Admin(admin_id) = ctx {
        if let Err(e) = tenancy::grant_membership(&state.db, admin_id, course.id).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    }

    // Best-effort: seed exercises from the linked repo's folders (GitHub only).
    // Failures here never fail course creation — they're reported as a note.
    let mut exercises_seeded = 0usize;
    let mut seed_note: Option<String> = None;
    let want_seed = body.seed_exercises.unwrap_or(true);
    if want_seed {
        if let Some(repo) = course.repo_url.as_deref() {
            match crate::repo::parse_github(repo) {
                Some((owner, name)) => {
                    // Repo-scoped App token first, then the shared token for
                    // allow-listed owners, then unauthenticated (public repos).
                    let credential = state.github.credential_for(&owner, &name).await;
                    let token = credential.token();
                    match crate::repo::discover_exercises(&owner, &name, token).await {
                        Ok(found) if !found.is_empty() => {
                            let items: Vec<(String, String, i32)> = found
                                .iter()
                                .enumerate()
                                .map(|(i, e)| (e.slug.clone(), e.title.clone(), i as i32))
                                .collect();
                            match crate::exercises::upsert(&state.db, course.id, &items).await {
                                Ok(()) => exercises_seeded = items.len(),
                                Err(e) => {
                                    seed_note = Some(format!("could not save exercises: {e}"))
                                }
                            }
                        }
                        Ok(_) => {}
                        // When a PAT exists but this owner isn't allow-listed (and no
                        // app token covered it), say so — otherwise the failure looks
                        // like a missing token.
                        Err(e) if credential.withheld() => {
                            seed_note = Some(format!(
                                "{e} — owner '{owner}' is not in HERMIONE_GITHUB_ALLOWED_OWNERS \
                                 and no GitHub App is installed on the repo, so no token was used"
                            ))
                        }
                        Err(e) => seed_note = Some(e),
                    }
                }
                None => seed_note = Some("exercise seeding supports GitHub repos only".to_string()),
            }
        }
    }

    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "slug": course.slug,
            "name": course.name,
            "repoUrl": course.repo_url,
            "enrollmentToken": course.enrollment_token,
            "exercisesSeeded": exercises_seeded,
            "seedNote": seed_note,
        })),
    )
        .into_response()
}

// --- course detail, update, membership -------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CourseDetailDto {
    slug: String,
    name: String,
    repo_url: Option<String>,
    enrollment_token: String,
    archived: bool,
    /// Where reference solutions live in the linked repo (see `solutions`).
    solutions_ref: Option<String>,
    solutions_dir: Option<String>,
    members: Vec<String>,
    description: Option<String>,
    term: Option<String>,
    institution: Option<String>,
    level: Option<String>,
}

/// GET /api/courses/{slug} — full detail incl. the enrollment token and members,
/// for the course-settings panel.
/// GET /api/courses/{slug}/tree — the linked repo's directory structure.
///
/// The tree view draws the repo itself rather than inferring folders from the
/// course's exercises: those are only ever the subset someone chose to define,
/// and are themselves often derived from the repo's `.hermione.json`.
///
/// Never an error status. A course with no repo, a non-GitHub URL, a private
/// repo without a token or a rate limit all return an empty list plus a note,
/// so the board degrades to the paths students actually have open instead of
/// showing an error where a tree should be.
pub(super) async fn course_tree(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(slug): Path<String>,
) -> Response {
    let course = match authorized_course(&state, ctx, &slug).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let note = |msg: &str| Json(serde_json::json!({ "dirs": [], "note": msg })).into_response();

    let Some(repo_url) = course.repo_url.as_deref() else {
        return note("no repository linked to this course");
    };
    let Some((owner, name)) = crate::repo::parse_github(repo_url) else {
        return note("the linked repository is not on GitHub");
    };

    let credential = state.github.credential_for(&owner, &name).await;
    let token = credential.token();

    match crate::repo::fetch_dirs(&owner, &name, token).await {
        Ok(dirs) => Json(serde_json::json!({ "dirs": dirs })).into_response(),
        Err(e) => note(&e),
    }
}

pub(super) async fn get_course(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(slug): Path<String>,
) -> Response {
    let course = match authorized_course(&state, ctx, &slug).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let members = match tenancy::admins_for_course(&state.db, course.id).await {
        Ok(rows) => rows.into_iter().map(|a| a.username).collect(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    Json(CourseDetailDto {
        slug: course.slug,
        name: course.name,
        repo_url: course.repo_url,
        enrollment_token: course.enrollment_token,
        archived: course.archived_at.is_some(),
        solutions_ref: course.solutions_ref,
        solutions_dir: course.solutions_dir,
        members,
        description: course.description,
        term: course.term,
        institution: course.institution,
        level: course.level,
    })
    .into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UpdateCourseBody {
    name: Option<String>,
    /// Present ⇒ set the linked repo; `null`/empty ⇒ clear it.
    #[serde(default, deserialize_with = "double_option")]
    repo_url: Option<Option<String>>,
    archived: Option<bool>,
    /// Where reference solutions live in the linked repo: a branch/tag/commit
    /// and a folder. Present ⇒ set, `null`/empty ⇒ clear.
    #[serde(default, deserialize_with = "double_option")]
    solutions_ref: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    solutions_dir: Option<Option<String>>,
    // Profile fields: present ⇒ set, `null`/empty ⇒ clear.
    #[serde(default, deserialize_with = "double_option")]
    description: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    term: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    institution: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    level: Option<Option<String>>,
}

/// A PATCH field (double-option): outer present ⇒ set/clear; trims, empty ⇒ clear.
fn patch_field(v: &Option<Option<String>>) -> Option<Option<String>> {
    v.as_ref().map(|inner| {
        inner
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

/// A PATCH field that must parse: like [`patch_field`], but a non-empty value is
/// run through `parse`, and the canonical form is what gets stored.
fn validated(
    v: &Option<Option<String>>,
    parse: impl Fn(&str) -> Result<String, crate::solutions::Invalid>,
) -> Result<Option<Option<String>>, crate::solutions::Invalid> {
    match patch_field(v) {
        Some(Some(raw)) => parse(&raw).map(|canonical| Some(Some(canonical))),
        other => Ok(other),
    }
}

/// PATCH /api/courses/{slug} — rename, relink the repo, edit the profile, or
/// (un)archive.
pub(super) async fn patch_course(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(slug): Path<String>,
    Json(body): Json<UpdateCourseBody>,
) -> Response {
    let course = match authorized_course(&state, ctx, &slug).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };

    // Reference-solution settings end up in requests to GitHub, so they are
    // parsed, and stored in their canonical form, or refused.
    let solutions_ref = match validated(&body.solutions_ref, |s| {
        crate::solutions::GitRef::parse(s).map(|r| r.to_string())
    }) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    let solutions_dir = match validated(&body.solutions_dir, |s| {
        crate::solutions::RepoPath::parse(s).map(|p| p.to_string())
    }) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    let patch = tenancy::CoursePatch {
        name: body
            .name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        repo_url: patch_field(&body.repo_url),
        solutions_ref,
        solutions_dir,
        description: patch_field(&body.description),
        term: patch_field(&body.term),
        institution: patch_field(&body.institution),
        level: patch_field(&body.level),
    };

    if !patch.is_empty() {
        if let Err(e) = tenancy::update_course(&state.db, course.id, &patch).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    }
    if let Some(archived) = body.archived {
        if let Err(e) = tenancy::set_course_archived(&state.db, course.id, archived).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

/// POST /api/courses/{slug}/rotate-token — issue a fresh enrollment token.
pub(super) async fn rotate_token(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(slug): Path<String>,
) -> Response {
    let course = match authorized_course(&state, ctx, &slug).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match tenancy::rotate_enrollment_token(&state.db, course.id).await {
        Ok(token) => Json(serde_json::json!({ "enrollmentToken": token })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct AddMemberBody {
    username: String,
}

/// POST /api/courses/{slug}/members — grant another existing admin access.
pub(super) async fn add_member(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(slug): Path<String>,
    Json(body): Json<AddMemberBody>,
) -> Response {
    let course = match authorized_course(&state, ctx, &slug).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let username = body.username.trim();
    let Some(admin_id) = tenancy::admin_by_username(&state.db, username).await else {
        return (
            StatusCode::NOT_FOUND,
            format!("no admin account named '{username}'"),
        )
            .into_response();
    };
    match tenancy::grant_membership(&state.db, admin_id, course.id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// DELETE /api/courses/{slug}/members/{username} — revoke access. Refuses to
/// remove the last member (which would orphan the course).
pub(super) async fn remove_member(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((slug, username)): Path<(String, String)>,
) -> Response {
    let course = match authorized_course(&state, ctx, &slug).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let Some(admin_id) = tenancy::admin_by_username(&state.db, username.trim()).await else {
        return (StatusCode::NOT_FOUND, "no such admin").into_response();
    };
    // Atomic: locks the membership rows so concurrent removals can't both slip
    // past the last-member check and orphan the course.
    match tenancy::revoke_membership_checked(&state.db, admin_id, course.id).await {
        Ok(tenancy::RevokeOutcome::Removed) => StatusCode::NO_CONTENT.into_response(),
        Ok(tenancy::RevokeOutcome::NotAMember) => {
            (StatusCode::NOT_FOUND, "not a member of this course").into_response()
        }
        Ok(tenancy::RevokeOutcome::LastMember) => (
            StatusCode::CONFLICT,
            "cannot remove the last member of a course",
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// serde helper: distinguishes an absent field from an explicit `null`, so PATCH
/// can tell "leave the repo alone" from "clear the repo".
fn double_option<'de, D, T>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Ok(Some(Option::deserialize(de)?))
}
