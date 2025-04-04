use cairo_lang_casm::builder::{CasmBuilder, Var};
use cairo_lang_casm::casm_build_extend;
use cairo_lang_sierra::extensions::array::{ArrayConcreteLibfunc, ConcreteMultiPopLibfunc};
use cairo_lang_sierra::extensions::gas::CostTokenType;
use cairo_lang_sierra::ids::ConcreteTypeId;

use super::{CompiledInvocation, CompiledInvocationBuilder, InvocationError};
use crate::invocations::{
    BuiltinInfo, CostValidationInfo, add_input_variables, get_non_fallthrough_statement_id,
};

/// Builds instructions for Sierra array operations.
pub fn build(
    libfunc: &ArrayConcreteLibfunc,
    builder: CompiledInvocationBuilder<'_>,
) -> Result<CompiledInvocation, InvocationError> {
    match libfunc {
        ArrayConcreteLibfunc::New(_) => build_array_new(builder),
        ArrayConcreteLibfunc::SpanFromTuple(libfunc) => build_span_from_tuple(builder, &libfunc.ty),
        ArrayConcreteLibfunc::TupleFromSpan(libfunc) => build_tuple_from_span(builder, &libfunc.ty),
        ArrayConcreteLibfunc::Append(_) => build_array_append(builder),
        ArrayConcreteLibfunc::PopFront(libfunc)
        | ArrayConcreteLibfunc::SnapshotPopFront(libfunc) => {
            build_pop_front(&libfunc.ty, builder, false)
        }
        ArrayConcreteLibfunc::PopFrontConsume(libfunc) => {
            build_pop_front(&libfunc.ty, builder, true)
        }
        ArrayConcreteLibfunc::SnapshotPopBack(libfunc) => {
            build_pop_back(&libfunc.ty, builder, false)
        }
        ArrayConcreteLibfunc::SnapshotMultiPopFront(libfunc) => {
            build_multi_pop_front(libfunc, builder)
        }
        ArrayConcreteLibfunc::SnapshotMultiPopBack(libfunc) => {
            build_multi_pop_back(libfunc, builder)
        }
        ArrayConcreteLibfunc::Get(libfunc) => build_array_get(&libfunc.ty, builder),
        ArrayConcreteLibfunc::Slice(libfunc) => build_array_slice(&libfunc.ty, builder),
        ArrayConcreteLibfunc::Len(libfunc) => build_array_len(&libfunc.ty, builder),
    }
}

/// Handles a Sierra statement for creating a new array.
fn build_array_new(
    builder: CompiledInvocationBuilder<'_>,
) -> Result<CompiledInvocation, InvocationError> {
    builder.try_get_refs::<0>()?;
    let mut casm_builder = CasmBuilder::default();
    casm_build_extend! {casm_builder,
        tempvar arr_start;
        hint AllocSegment into {dst: arr_start};
        ap += 1;
    };
    Ok(builder.build_from_casm_builder(
        casm_builder,
        [("Fallthrough", &[&[arr_start, arr_start]], None)],
        Default::default(),
    ))
}

/// Builds instructions for converting a box of a struct containing only the same type as members
/// into a span of that type.
fn build_span_from_tuple(
    builder: CompiledInvocationBuilder<'_>,
    ty: &ConcreteTypeId,
) -> Result<CompiledInvocation, InvocationError> {
    let [start_ptr] = builder.try_get_single_cells()?;
    let full_struct_size = builder.program_info.type_sizes[ty];

    let mut casm_builder = CasmBuilder::default();

    add_input_variables! {casm_builder,
        deref start_ptr;
    };
    casm_build_extend! {casm_builder,
        let arr_start = start_ptr;
        // As the actual size of the span is exactly the same as the wrapping struct.
        const span_size_in_cells = full_struct_size;
        let arr_end = arr_start + span_size_in_cells;
    };
    Ok(builder.build_from_casm_builder(
        casm_builder,
        [("Fallthrough", &[&[arr_start, arr_end]], None)],
        Default::default(),
    ))
}

/// Builds instructions for converting a span of a type into a box of a struct containing only
/// members of that same type.
fn build_tuple_from_span(
    builder: CompiledInvocationBuilder<'_>,
    ty: &ConcreteTypeId,
) -> Result<CompiledInvocation, InvocationError> {
    let [arr_start, arr_end] = builder.try_get_refs::<1>()?[0].try_unpack()?;
    let full_struct_size = builder.program_info.type_sizes[ty];

    let mut casm_builder = CasmBuilder::default();

    add_input_variables! {casm_builder,
        deref arr_start;
        deref arr_end;
    };
    casm_build_extend! {casm_builder,
        const success_span_size = full_struct_size;
        tempvar actual_length = arr_end - arr_start;
        tempvar diff = actual_length - success_span_size;
        jump Failure if diff != 0;
    };
    let failure_handle = get_non_fallthrough_statement_id(&builder);
    Ok(builder.build_from_casm_builder(
        casm_builder,
        [("Fallthrough", &[&[arr_start]], None), ("Failure", &[], Some(failure_handle))],
        Default::default(),
    ))
}

/// Handles a Sierra statement for appending an element to an array.
fn build_array_append(
    builder: CompiledInvocationBuilder<'_>,
) -> Result<CompiledInvocation, InvocationError> {
    let [expr_arr, elem] = builder.try_get_refs()?;
    let [arr_start, arr_end] = expr_arr.try_unpack()?;

    let mut casm_builder = CasmBuilder::default();
    add_input_variables! {casm_builder,
        buffer(0) arr_start;
        buffer(elem.cells.len() as i16) arr_end;
    };
    for cell in &elem.cells {
        add_input_variables!(casm_builder, deref cell;);
        casm_build_extend!(casm_builder, assert cell = *(arr_end++););
    }
    Ok(builder.build_from_casm_builder(
        casm_builder,
        [("Fallthrough", &[&[arr_start, arr_end]], None)],
        Default::default(),
    ))
}

/// Handles a Sierra statement for popping an element from the beginning of an array.
fn build_pop_front(
    elem_ty: &ConcreteTypeId,
    builder: CompiledInvocationBuilder<'_>,
    consume: bool,
) -> Result<CompiledInvocation, InvocationError> {
    let [arr_start, arr_end] = builder.try_get_refs::<1>()?[0].try_unpack()?;
    let element_size = builder.program_info.type_sizes[elem_ty];

    let mut casm_builder = CasmBuilder::default();
    add_input_variables! {casm_builder,
        deref arr_start;
        deref arr_end;
    };
    casm_build_extend! {casm_builder,
        tempvar is_non_empty = arr_end - arr_start;
        jump NonEmpty if is_non_empty != 0;
        jump Failure;
        NonEmpty:
        const element_size_imm = element_size;
        let new_start = arr_start + element_size_imm;
    };
    let failure_handle = get_non_fallthrough_statement_id(&builder);
    let arr_arr = [arr_start, arr_end];
    let failure_ret = if consume { vec![] } else { vec![&arr_arr[..]] };
    Ok(builder.build_from_casm_builder(
        casm_builder,
        [
            ("Fallthrough", &[&[new_start, arr_end], &[arr_start]], None),
            ("Failure", &failure_ret[..], Some(failure_handle)),
        ],
        Default::default(),
    ))
}

/// Handles a Sierra statement for popping an element from the end of an array.
fn build_pop_back(
    elem_ty: &ConcreteTypeId,
    builder: CompiledInvocationBuilder<'_>,
    consume: bool,
) -> Result<CompiledInvocation, InvocationError> {
    let [arr_start, arr_end] = builder.try_get_refs::<1>()?[0].try_unpack()?;
    let element_size = builder.program_info.type_sizes[elem_ty];

    let mut casm_builder = CasmBuilder::default();
    add_input_variables! {casm_builder,
        deref arr_start;
        deref arr_end;
    };
    casm_build_extend! {casm_builder,
        tempvar is_non_empty = arr_end - arr_start;
        jump NonEmpty if is_non_empty != 0;
        jump Failure;
        NonEmpty:
        const element_size_imm = element_size;
        let new_end = arr_end - element_size_imm;
    };
    let failure_handle = get_non_fallthrough_statement_id(&builder);
    let arr_arr = [arr_start, arr_end];
    let failure_ret = if consume { vec![] } else { vec![&arr_arr[..]] };
    Ok(builder.build_from_casm_builder(
        casm_builder,
        [
            ("Fallthrough", &[&[arr_start, new_end], &[new_end]], None),
            ("Failure", &failure_ret[..], Some(failure_handle)),
        ],
        Default::default(),
    ))
}

/// Handles a Sierra statement for fetching an array element at a specific index.
fn build_array_get(
    elem_ty: &ConcreteTypeId,
    builder: CompiledInvocationBuilder<'_>,
) -> Result<CompiledInvocation, InvocationError> {
    let [expr_range_check, expr_arr, expr_index] = builder.try_get_refs()?;
    let range_check = expr_range_check.try_unpack_single()?;
    let [arr_start, arr_end] = expr_arr.try_unpack()?;
    let index = expr_index.try_unpack_single()?;

    let element_size = builder.program_info.type_sizes[elem_ty];

    let mut casm_builder = CasmBuilder::default();
    add_input_variables! {casm_builder,
        deref_or_immediate index;
        deref arr_start;
        deref arr_end;
        buffer(1) range_check;
    };
    casm_build_extend! {casm_builder,
        const element_size = element_size;
        let orig_range_check = range_check;
        // Compute the length of the array (in cells).
        tempvar array_length_in_cells = arr_end - arr_start;
        // Compute the offset of the element (in cells).
        maybe_tempvar element_offset_in_cells = index * element_size;
        // Check that offset is in range.
        // Note that the offset may be as large as `(2^15 - 1) * (2^32 - 1)`.
        tempvar is_in_range;
        hint TestLessThan {lhs: element_offset_in_cells, rhs: array_length_in_cells} into {dst: is_in_range};
        jump InRange if is_in_range != 0;
        // Index out of bounds. Compute offset - length.
        tempvar offset_length_diff = element_offset_in_cells - array_length_in_cells;
        // Assert offset - length >= 0. Note that offset_length_diff is smaller than 2^128 as the index type is u32.
        assert offset_length_diff  = *(range_check++);
        jump FailureHandle;

        InRange:
        // Assert offset < length, or that length - (offset + 1) is in [0, 2^128).
        // Compute offset + 1.
        const one = 1;
        tempvar element_offset_in_cells_plus_1 = element_offset_in_cells + one;
        // Compute length - (offset + 1).
        tempvar offset_length_diff = array_length_in_cells - element_offset_in_cells_plus_1;
        // Assert length - (offset + 1) is in [0, 2^128).
        assert offset_length_diff = *(range_check++);
         // The start address of target cells.
        let target_cell = arr_start + element_offset_in_cells;
    };
    let failure_handle = get_non_fallthrough_statement_id(&builder);
    Ok(builder.build_from_casm_builder(
        casm_builder,
        [
            ("Fallthrough", &[&[range_check], &[target_cell]], None),
            ("FailureHandle", &[&[range_check]], Some(failure_handle)),
        ],
        CostValidationInfo {
            builtin_infos: vec![BuiltinInfo {
                cost_token_ty: CostTokenType::RangeCheck,
                start: orig_range_check,
                end: range_check,
            }],
            extra_costs: None,
        },
    ))
}

/// Handles a Sierra statement for returning a snapshot of a slice of an array.
fn build_array_slice(
    elem_ty: &ConcreteTypeId,
    builder: CompiledInvocationBuilder<'_>,
) -> Result<CompiledInvocation, InvocationError> {
    let [expr_range_check, expr_arr, expr_slice_start, expr_slice_length] =
        builder.try_get_refs()?;
    let range_check = expr_range_check.try_unpack_single()?;
    let [arr_start, arr_end] = expr_arr.try_unpack()?;
    let slice_start = expr_slice_start.try_unpack_single()?;
    let slice_length = expr_slice_length.try_unpack_single()?;

    let element_size = builder.program_info.type_sizes[elem_ty];

    let mut casm_builder = CasmBuilder::default();
    add_input_variables! {casm_builder,
        deref slice_start;
        deref_or_immediate slice_length;
        deref arr_start;
        deref arr_end;
        buffer(1) range_check;
    };
    casm_build_extend! {casm_builder,
        let orig_range_check = range_check;
        const element_size = element_size;
        // Compute the length of the array (in cells).
        tempvar array_length_in_cells = arr_end - arr_start;
        tempvar slice_end = slice_start + slice_length;
        // Compute the offset of the element (in cells).
        maybe_tempvar slice_end_in_cells = slice_end * element_size;
        // Check that offset is in range.
        // Note that the offset may be as large as `(2^15 - 1) * (2^32 - 1) * 2`.
        tempvar is_in_range;
        hint TestLessThanOrEqual {lhs: slice_end_in_cells, rhs: array_length_in_cells} into {dst: is_in_range};
        jump InRange if is_in_range != 0;
        // Index out of bounds. Assert that end_offset > length or that end_offset - 1 >= length or that (end_offset - 1 - length) in [0, 2^128).
        // Compute length + 1.
        const one = 1;
        tempvar length_plus_1 = array_length_in_cells + one;
        // Compute the diff.
        tempvar offset_length_diff = slice_end_in_cells - length_plus_1;
        // Range check the diff.
        assert offset_length_diff  = *(range_check++);
        jump FailureHandle;

        InRange:
        // Assert end_offset <= length, or that length - end_offset is in [0, 2^128).
        // Compute length - end_offset.
        tempvar offset_length_diff = array_length_in_cells - slice_end_in_cells;
        // Assert length - end_offset >= 0. Note that offset_length_diff is smaller than 2^128 as the index type is u32.
        assert offset_length_diff = *(range_check++);
        // Compute the offset of the element (in cells).
        maybe_tempvar slice_start_in_cells = slice_start * element_size;
        let slice_start_cell = arr_start + slice_start_in_cells;
        let slice_end_cell = arr_start + slice_end_in_cells;
    };
    let failure_handle = get_non_fallthrough_statement_id(&builder);
    Ok(builder.build_from_casm_builder(
        casm_builder,
        [
            ("Fallthrough", &[&[range_check], &[slice_start_cell, slice_end_cell]], None),
            ("FailureHandle", &[&[range_check]], Some(failure_handle)),
        ],
        CostValidationInfo {
            builtin_infos: vec![BuiltinInfo {
                cost_token_ty: CostTokenType::RangeCheck,
                start: orig_range_check,
                end: range_check,
            }],
            extra_costs: None,
        },
    ))
}

/// Handles a Sierra statement for getting the length of an array.
fn build_array_len(
    elem_ty: &ConcreteTypeId,
    builder: CompiledInvocationBuilder<'_>,
) -> Result<CompiledInvocation, InvocationError> {
    let [arr_start, arr_end] = builder.try_get_refs::<1>()?[0].try_unpack()?;
    let element_size = builder.program_info.type_sizes[elem_ty];
    let mut casm_builder = CasmBuilder::default();
    add_input_variables! {casm_builder,
        deref arr_start;
        deref arr_end;
    };
    let length = if element_size == 1 {
        casm_build_extend! {casm_builder,
            let length = arr_end - arr_start;
        };
        length
    } else {
        casm_build_extend! {casm_builder,
            tempvar end_total_offset = arr_end - arr_start;
            const element_size = element_size;
            let length = end_total_offset / element_size;
        };
        length
    };
    Ok(builder.build_from_casm_builder(
        casm_builder,
        [("Fallthrough", &[&[length]], None)],
        Default::default(),
    ))
}

/// Handles a Sierra statement for popping elements from the beginning of an array.
fn build_multi_pop_front(
    libfunc: &ConcreteMultiPopLibfunc,
    builder: CompiledInvocationBuilder<'_>,
) -> Result<CompiledInvocation, InvocationError> {
    let [expr_range_check, expr_arr] = builder.try_get_refs()?;
    let range_check = expr_range_check.try_unpack_single()?;
    let [arr_start, arr_end] = expr_arr.try_unpack()?;
    let popped_size = builder.program_info.type_sizes[&libfunc.popped_ty];

    let mut casm_builder = CasmBuilder::default();
    add_input_variables! {casm_builder,
        buffer(1) range_check;
        deref arr_start;
        deref arr_end;
    };
    casm_build_extend!(casm_builder, let orig_range_check = range_check;);
    extend_multi_pop_failure_checks(
        &mut casm_builder,
        range_check,
        arr_start,
        arr_end,
        popped_size,
    );
    casm_build_extend! {casm_builder,
        // Success case.
        const popped_size = popped_size;
        tempvar rc;
        // Calculating the new start, as it is required for calculating the range checked value.
        tempvar new_start = arr_start + popped_size;
        assert rc = arr_end - new_start;
        assert rc = *(range_check++);
    };
    let failure_handle = get_non_fallthrough_statement_id(&builder);
    Ok(builder.build_from_casm_builder(
        casm_builder,
        [
            ("Fallthrough", &[&[range_check], &[new_start, arr_end], &[arr_start]], None),
            ("Failure", &[&[range_check], &[arr_start, arr_end]], Some(failure_handle)),
        ],
        CostValidationInfo {
            builtin_infos: vec![BuiltinInfo {
                cost_token_ty: CostTokenType::RangeCheck,
                start: orig_range_check,
                end: range_check,
            }],
            extra_costs: None,
        },
    ))
}

/// Handles a Sierra statement for popping elements from the end of an array.
fn build_multi_pop_back(
    libfunc: &ConcreteMultiPopLibfunc,
    builder: CompiledInvocationBuilder<'_>,
) -> Result<CompiledInvocation, InvocationError> {
    let [expr_range_check, expr_arr] = builder.try_get_refs()?;
    let range_check = expr_range_check.try_unpack_single()?;
    let [arr_start, arr_end] = expr_arr.try_unpack()?;
    let popped_size = builder.program_info.type_sizes[&libfunc.popped_ty];

    let mut casm_builder = CasmBuilder::default();
    add_input_variables! {casm_builder,
        buffer(1) range_check;
        deref arr_start;
        deref arr_end;
    };
    casm_build_extend!(casm_builder, let orig_range_check = range_check;);
    extend_multi_pop_failure_checks(
        &mut casm_builder,
        range_check,
        arr_start,
        arr_end,
        popped_size,
    );
    casm_build_extend! {casm_builder,
        // Success case.
        const popped_size = popped_size;
        tempvar rc;
        // Calculating the new end, as it is required for calculating the range checked value.
        tempvar new_end = arr_end - popped_size;
        assert rc = new_end - arr_start;
        assert rc = *(range_check++);
    };
    let failure_handle = get_non_fallthrough_statement_id(&builder);
    Ok(builder.build_from_casm_builder(
        casm_builder,
        [
            ("Fallthrough", &[&[range_check], &[arr_start, new_end], &[new_end]], None),
            ("Failure", &[&[range_check], &[arr_start, arr_end]], Some(failure_handle)),
        ],
        CostValidationInfo {
            builtin_infos: vec![BuiltinInfo {
                cost_token_ty: CostTokenType::RangeCheck,
                start: orig_range_check,
                end: range_check,
            }],
            extra_costs: None,
        },
    ))
}

/// Extends the casm builder with the common part of the multi-pop front and multi-pop back.
fn extend_multi_pop_failure_checks(
    casm_builder: &mut CasmBuilder,
    range_check: Var,
    arr_start: Var,
    arr_end: Var,
    popped_size: i16,
) {
    casm_build_extend! {casm_builder,
        const popped_size_minus_1 = popped_size - 1;
        const popped_size = popped_size;
        let arr_start_popped = arr_start + popped_size;
        tempvar has_enough_elements;
        hint TestLessThanOrEqualAddress {
            lhs: arr_start_popped, rhs: arr_end
        } into { dst: has_enough_elements };
        jump HasEnoughElements if has_enough_elements != 0;
        // Check that `arr_start - arr_end + popped_size - 1 >= 0`.
        // This implies `popped_size - 1 >= arr_end - arr_start = arr_size` ==>
        // `arr_size < popped_size`.
        tempvar minus_size = arr_start - arr_end;
        tempvar rc = minus_size + popped_size_minus_1;
        assert rc = *(range_check++);
        jump Failure;
        HasEnoughElements:
    };
}
