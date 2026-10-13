alter table projects
    add column created_by uuid references users(id);
