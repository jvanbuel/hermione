//! Multi-tenant building blocks: admin accounts, courses, and membership.

use argon2::password_hash::{rand_core::OsRng, SaltString};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use chrono::Utc;
use hermione_entity::{admins, course_admins, courses};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, DatabaseConnection, DbErr, EntityTrait, PaginatorTrait,
    QueryFilter,
};
use uuid::Uuid;

/// The seeded default course (open dev mode + adopted pre-tenancy data).
pub const DEFAULT_COURSE_ID: Uuid = Uuid::from_u128(1);

// --- password hashing ------------------------------------------------------

pub fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

// --- admins ----------------------------------------------------------------

pub async fn create_admin(
    db: &DatabaseConnection,
    username: &str,
    password: &str,
) -> Result<admins::Model, String> {
    // Argon2 is intentionally CPU-heavy; keep it off the async worker threads.
    let owned = password.to_string();
    let hash = tokio::task::spawn_blocking(move || hash_password(&owned))
        .await
        .map_err(|e| e.to_string())??;
    let model = admins::ActiveModel {
        id: Set(Uuid::new_v4()),
        username: Set(username.to_string()),
        password_hash: Set(hash),
        created_at: Set(Utc::now().into()),
    };
    admins::Entity::insert(model)
        .exec_with_returning(db)
        .await
        .map_err(|e| e.to_string())
}

/// Returns the admin id if the credentials are valid.
pub async fn verify_login(db: &DatabaseConnection, username: &str, password: &str) -> Option<Uuid> {
    let admin = admins::Entity::find()
        .filter(admins::Column::Username.eq(username))
        .one(db)
        .await
        .ok()??;
    let (owned, stored) = (password.to_string(), admin.password_hash.clone());
    let ok = tokio::task::spawn_blocking(move || verify_password(&owned, &stored))
        .await
        .unwrap_or(false);
    ok.then_some(admin.id)
}

pub async fn count_admins(db: &DatabaseConnection) -> u64 {
    admins::Entity::find().count(db).await.unwrap_or(0)
}

pub async fn admin_by_username(db: &DatabaseConnection, username: &str) -> Option<Uuid> {
    admins::Entity::find()
        .filter(admins::Column::Username.eq(username))
        .one(db)
        .await
        .ok()
        .flatten()
        .map(|a| a.id)
}

// --- courses ---------------------------------------------------------------

/// Filters courses by archived state: active (`archived_at IS NULL`) or archived.
fn archived_filter(archived: bool) -> sea_orm::sea_query::SimpleExpr {
    if archived {
        courses::Column::ArchivedAt.is_not_null()
    } else {
        courses::Column::ArchivedAt.is_null()
    }
}

pub async fn create_course(
    db: &DatabaseConnection,
    slug: &str,
    name: &str,
    repo_url: Option<&str>,
) -> Result<courses::Model, String> {
    let model = courses::ActiveModel {
        id: Set(Uuid::new_v4()),
        slug: Set(slug.to_string()),
        name: Set(name.to_string()),
        enrollment_token: Set(Uuid::new_v4().simple().to_string()),
        repo_url: Set(repo_url.map(str::to_string)),
        archived_at: Set(None),
        created_at: Set(Utc::now().into()),
    };
    courses::Entity::insert(model)
        .exec_with_returning(db)
        .await
        .map_err(|e| e.to_string())
}

/// Updates a course's name and/or linked repo. `name` is applied when `Some`;
/// `repo_url` is applied when `Some` (inner `None` clears the link).
pub async fn update_course(
    db: &DatabaseConnection,
    course_id: Uuid,
    name: Option<&str>,
    repo_url: Option<Option<&str>>,
) -> Result<courses::Model, DbErr> {
    let mut model = courses::ActiveModel {
        id: Set(course_id),
        ..Default::default()
    };
    if let Some(name) = name {
        model.name = Set(name.to_string());
    }
    if let Some(repo_url) = repo_url {
        model.repo_url = Set(repo_url.map(str::to_string));
    }
    courses::Entity::update(model).exec(db).await
}

/// Archives or restores a course. Archived courses drop out of the active
/// switcher but keep all their data.
pub async fn set_course_archived(
    db: &DatabaseConnection,
    course_id: Uuid,
    archived: bool,
) -> Result<courses::Model, DbErr> {
    let model = courses::ActiveModel {
        id: Set(course_id),
        archived_at: Set(archived.then(|| Utc::now().into())),
        ..Default::default()
    };
    courses::Entity::update(model).exec(db).await
}

/// Issues a fresh enrollment token for a course (invalidating the old one).
pub async fn rotate_enrollment_token(
    db: &DatabaseConnection,
    course_id: Uuid,
) -> Result<String, DbErr> {
    let token = Uuid::new_v4().simple().to_string();
    let model = courses::ActiveModel {
        id: Set(course_id),
        enrollment_token: Set(token.clone()),
        ..Default::default()
    };
    courses::Entity::update(model).exec(db).await?;
    Ok(token)
}

pub async fn course_by_slug(db: &DatabaseConnection, slug: &str) -> Option<courses::Model> {
    courses::Entity::find()
        .filter(courses::Column::Slug.eq(slug))
        .one(db)
        .await
        .ok()
        .flatten()
}

pub async fn course_by_token(db: &DatabaseConnection, token: &str) -> Option<courses::Model> {
    courses::Entity::find()
        .filter(courses::Column::EnrollmentToken.eq(token))
        .one(db)
        .await
        .ok()
        .flatten()
}

// --- membership ------------------------------------------------------------

pub async fn grant_membership(
    db: &DatabaseConnection,
    admin_id: Uuid,
    course_id: Uuid,
) -> Result<(), DbErr> {
    if course_admins::Entity::find_by_id((admin_id, course_id))
        .one(db)
        .await?
        .is_some()
    {
        return Ok(());
    }
    let model = course_admins::ActiveModel {
        admin_id: Set(admin_id),
        course_id: Set(course_id),
    };
    course_admins::Entity::insert(model).exec(db).await?;
    Ok(())
}

pub async fn is_member(db: &DatabaseConnection, admin_id: Uuid, course_id: Uuid) -> bool {
    course_admins::Entity::find_by_id((admin_id, course_id))
        .one(db)
        .await
        .ok()
        .flatten()
        .is_some()
}

/// Removes an admin's access to a course. Idempotent.
pub async fn revoke_membership(
    db: &DatabaseConnection,
    admin_id: Uuid,
    course_id: Uuid,
) -> Result<(), DbErr> {
    course_admins::Entity::delete_by_id((admin_id, course_id))
        .exec(db)
        .await?;
    Ok(())
}

/// The admins with access to a course, ordered by username.
pub async fn admins_for_course(
    db: &DatabaseConnection,
    course_id: Uuid,
) -> Result<Vec<admins::Model>, DbErr> {
    let admin_ids: Vec<Uuid> = course_admins::Entity::find()
        .filter(course_admins::Column::CourseId.eq(course_id))
        .all(db)
        .await?
        .into_iter()
        .map(|m| m.admin_id)
        .collect();
    if admin_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut rows = admins::Entity::find()
        .filter(admins::Column::Id.is_in(admin_ids))
        .all(db)
        .await?;
    rows.sort_by(|a, b| a.username.cmp(&b.username));
    Ok(rows)
}

/// How many admins have access to a course.
pub async fn count_course_admins(db: &DatabaseConnection, course_id: Uuid) -> Result<u64, DbErr> {
    course_admins::Entity::find()
        .filter(course_admins::Column::CourseId.eq(course_id))
        .count(db)
        .await
}

/// Courses an admin may access. `archived` selects active (false) or archived
/// (true) courses.
pub async fn courses_for_admin(
    db: &DatabaseConnection,
    admin_id: Uuid,
    archived: bool,
) -> Result<Vec<courses::Model>, DbErr> {
    let course_ids: Vec<Uuid> = course_admins::Entity::find()
        .filter(course_admins::Column::AdminId.eq(admin_id))
        .all(db)
        .await?
        .into_iter()
        .map(|m| m.course_id)
        .collect();
    if course_ids.is_empty() {
        return Ok(Vec::new());
    }
    courses::Entity::find()
        .filter(courses::Column::Id.is_in(course_ids))
        .filter(archived_filter(archived))
        .all(db)
        .await
}

pub async fn all_courses(db: &DatabaseConnection, archived: bool) -> Result<Vec<courses::Model>, DbErr> {
    courses::Entity::find()
        .filter(archived_filter(archived))
        .all(db)
        .await
}
