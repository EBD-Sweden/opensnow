-- Staging model: clean and type-cast raw call detail records (CDRs).
--
-- Source table `raw_cdrs` is assumed to exist in OpenSnow with columns:
--   call_id, caller_number, callee_number, call_start, call_end,
--   duration_seconds, call_type, cell_tower_id, status

with source as (

    select * from {{ source('telecom', 'raw_cdrs') }}

),

renamed as (

    select
        cast(call_id as bigint)                     as call_id,
        trim(caller_number)                         as caller_number,
        trim(callee_number)                         as callee_number,
        cast(call_start as timestamp)               as call_started_at,
        cast(call_end as timestamp)                 as call_ended_at,
        cast(duration_seconds as integer)            as duration_seconds,
        lower(trim(call_type))                       as call_type,
        trim(cell_tower_id)                          as cell_tower_id,
        lower(trim(status))                          as call_status,
        cast(call_start as date)                     as call_date

    from source
    where call_id is not null

)

select * from renamed
